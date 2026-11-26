//! The JSON form of axioms, which the HTTP API reads and writes.
//!
//! An axiom is an object whose `type` is its functional-style name, with named members
//! for its operands and an optional `annotations` list. A class, a datatype, a property
//! or a named individual in a position that fixes its kind is an IRI string, an
//! anonymous individual is `"_:label"`, and an inverse property is
//! `{"inverseOf": IRI}`. A literal is `{"@value": …}` with `@type` or `@language`, as in
//! JSON-LD. Class expressions and data ranges other than entities are objects with a
//! `type` as well.
//!
//! ```json
//! {"type": "SubClassOf", "subClass": "http://ex.org/A",
//!  "superClass": {"type": "ObjectSomeValuesFrom", "property": "http://ex.org/p",
//!                 "class": "http://ex.org/B"},
//!  "annotations": [{"property": "http://www.w3.org/2000/01/rdf-schema#comment",
//!                   "value": {"@value": "every A has a p that is a B"}}]}
//! ```

use crate::model::*;
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

type R<T> = Result<T, ModelError>;

fn err<T>(m: impl Into<String>) -> R<T> {
    Err(ModelError(m.into()))
}

// ------------------------------------------------------------------ writing ------

fn iri_v(i: &Iri) -> Value {
    Value::String(i.as_str().to_string())
}

pub fn literal(l: &Literal) -> Value {
    let mut m = Map::new();
    m.insert("@value".into(), l.value().into());
    if let Some(lang) = l.language() {
        m.insert("@language".into(), lang.into());
    } else if l.datatype() != crate::vocab::XSD_STRING {
        m.insert("@type".into(), l.datatype().into());
    }
    Value::Object(m)
}

pub fn individual(i: &Individual) -> Value {
    match i {
        Individual::Named(n) => iri_v(&n.0),
        Individual::Anonymous(a) => format!("_:{}", a.label()).into(),
    }
}

pub fn ope(p: &ObjectPropertyExpression) -> Value {
    match p {
        ObjectPropertyExpression::Property(p) => iri_v(&p.0),
        ObjectPropertyExpression::InverseOf(p) => json!({ "inverseOf": p.as_str() }),
    }
}

pub fn data_range(r: &DataRange) -> Value {
    match r {
        DataRange::Datatype(d) => iri_v(&d.0),
        DataRange::IntersectionOf(rs) => {
            json!({"type": "DataIntersectionOf", "ranges": rs.iter().map(data_range).collect::<Vec<_>>()})
        }
        DataRange::UnionOf(rs) => {
            json!({"type": "DataUnionOf", "ranges": rs.iter().map(data_range).collect::<Vec<_>>()})
        }
        DataRange::ComplementOf(r) => json!({"type": "DataComplementOf", "range": data_range(r)}),
        DataRange::OneOf(ls) => {
            json!({"type": "DataOneOf", "values": ls.iter().map(literal).collect::<Vec<_>>()})
        }
        DataRange::DatatypeRestriction(d, fs) => json!({
            "type": "DatatypeRestriction",
            "datatype": d.as_str(),
            "restrictions": fs.iter().map(|f| json!({"facet": f.facet.as_str(), "value": literal(&f.value)})).collect::<Vec<_>>(),
        }),
    }
}

pub fn class_expression(c: &ClassExpression) -> Value {
    use ClassExpression as C;
    let t = c.type_name();
    let card = |n: &u32, p: Value, q: Option<(&str, Value)>| {
        let mut m = Map::new();
        m.insert("type".into(), t.into());
        m.insert("cardinality".into(), (*n).into());
        m.insert("property".into(), p);
        if let Some((k, v)) = q {
            m.insert(k.into(), v);
        }
        Value::Object(m)
    };
    let props = |ps: &[DataProperty]| -> (&'static str, Value) {
        if ps.len() == 1 {
            ("property", iri_v(&ps[0].0))
        } else {
            ("properties", ps.iter().map(|p| iri_v(&p.0)).collect())
        }
    };
    match c {
        C::Class(c) => iri_v(&c.0),
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
            json!({"type": t, "classes": cs.iter().map(class_expression).collect::<Vec<_>>()})
        }
        C::ObjectComplementOf(c) => json!({"type": t, "class": class_expression(c)}),
        C::ObjectOneOf(is) => {
            json!({"type": t, "individuals": is.iter().map(individual).collect::<Vec<_>>()})
        }
        C::ObjectSomeValuesFrom(p, c) | C::ObjectAllValuesFrom(p, c) => {
            json!({"type": t, "property": ope(p), "class": class_expression(c)})
        }
        C::ObjectHasValue(p, i) => {
            json!({"type": t, "property": ope(p), "individual": individual(i)})
        }
        C::ObjectHasSelf(p) => json!({"type": t, "property": ope(p)}),
        C::ObjectMinCardinality(n, p, f)
        | C::ObjectMaxCardinality(n, p, f)
        | C::ObjectExactCardinality(n, p, f) => card(
            n,
            ope(p),
            f.as_ref().map(|f| ("class", class_expression(f))),
        ),
        C::DataSomeValuesFrom(ps, r) | C::DataAllValuesFrom(ps, r) => {
            let (k, v) = props(ps);
            let mut m = Map::new();
            m.insert("type".into(), t.into());
            m.insert(k.into(), v);
            m.insert("range".into(), data_range(r));
            Value::Object(m)
        }
        C::DataHasValue(p, l) => json!({"type": t, "property": p.as_str(), "value": literal(l)}),
        C::DataMinCardinality(n, p, r)
        | C::DataMaxCardinality(n, p, r)
        | C::DataExactCardinality(n, p, r) => {
            card(n, iri_v(&p.0), r.as_ref().map(|r| ("range", data_range(r))))
        }
    }
}

fn annotation_value(v: &AnnotationValue) -> Value {
    match v {
        AnnotationValue::Iri(i) => iri_v(i),
        AnnotationValue::Literal(l) => literal(l),
        AnnotationValue::Anonymous(a) => format!("_:{}", a.label()).into(),
    }
}

pub fn annotation(a: &Annotation) -> Value {
    let mut m = Map::new();
    m.insert("property".into(), a.property.as_str().into());
    m.insert("value".into(), annotation_value(&a.value));
    if !a.annotations.is_empty() {
        m.insert(
            "annotations".into(),
            a.annotations.iter().map(annotation).collect(),
        );
    }
    Value::Object(m)
}

/// Conservative zero-allocation lower bound check on whether the JSON encoding of `ax`
/// could fit within `max_bytes`. Returns `false` immediately once the lower bound exceeds
/// `max_bytes`, avoiding allocating `serde_json::Value` trees for entries that will be
/// omitted from a size-capped response.
pub fn axiom_fits_bytes(ax: &Axiom, max_bytes: usize) -> bool {
    struct Counter {
        used: usize,
        limit: usize,
    }
    impl Counter {
        fn add(&mut self, n: usize) -> bool {
            self.used = self.used.saturating_add(n);
            self.used <= self.limit
        }
        fn iri(&mut self, s: &str) -> bool {
            self.add(s.len().saturating_add(2))
        }
        fn lit(&mut self, l: &Literal) -> bool {
            let mut n = l.value().len().saturating_add(13); // {"@value":""}
            if let Some(lang) = l.language() {
                n = n.saturating_add(lang.len()).saturating_add(15); // ,"@language":""
            } else if l.datatype() != crate::vocab::XSD_STRING {
                n = n.saturating_add(l.datatype().len()).saturating_add(11); // ,"@type":""
            }
            self.add(n)
        }
        fn ind(&mut self, i: &Individual) -> bool {
            match i {
                Individual::Named(n) => self.iri(n.as_str()),
                Individual::Anonymous(a) => self.add(a.label().len().saturating_add(4)),
            }
        }
        fn ope(&mut self, p: &ObjectPropertyExpression) -> bool {
            match p {
                ObjectPropertyExpression::Property(p) => self.iri(p.as_str()),
                ObjectPropertyExpression::InverseOf(p) => {
                    self.add(p.as_str().len().saturating_add(16))
                }
            }
        }
        fn dr(&mut self, r: &DataRange) -> bool {
            match r {
                DataRange::Datatype(d) => self.iri(d.as_str()),
                DataRange::IntersectionOf(rs) | DataRange::UnionOf(rs) => {
                    self.add(32) && rs.iter().all(|x| self.dr(x) && self.add(1))
                }
                DataRange::ComplementOf(r) => self.add(36) && self.dr(r),
                DataRange::OneOf(ls) => {
                    self.add(30) && ls.iter().all(|l| self.lit(l) && self.add(1))
                }
                DataRange::DatatypeRestriction(d, fs) => {
                    self.add(52)
                        && self.iri(d.as_str())
                        && fs.iter().all(|f| {
                            self.add(22) && self.iri(f.facet.as_str()) && self.lit(&f.value)
                        })
                }
            }
        }
        fn ce(&mut self, c: &ClassExpression) -> bool {
            use ClassExpression as C;
            match c {
                C::Class(c) => self.iri(c.as_str()),
                C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
                    self.add(36) && cs.iter().all(|x| self.ce(x) && self.add(1))
                }
                C::ObjectComplementOf(c) => self.add(38) && self.ce(c),
                C::ObjectOneOf(is) => self.add(36) && is.iter().all(|i| self.ind(i) && self.add(1)),
                C::ObjectSomeValuesFrom(p, c) | C::ObjectAllValuesFrom(p, c) => {
                    self.add(48) && self.ope(p) && self.ce(c)
                }
                C::ObjectHasValue(p, i) => self.add(48) && self.ope(p) && self.ind(i),
                C::ObjectHasSelf(p) => self.add(36) && self.ope(p),
                C::ObjectMinCardinality(_, p, f)
                | C::ObjectMaxCardinality(_, p, f)
                | C::ObjectExactCardinality(_, p, f) => {
                    self.add(56) && self.ope(p) && f.as_ref().is_none_or(|x| self.ce(x))
                }
                C::DataSomeValuesFrom(ps, r) | C::DataAllValuesFrom(ps, r) => {
                    self.add(46) && ps.iter().all(|p| self.iri(p.as_str())) && self.dr(r)
                }
                C::DataHasValue(p, l) => self.add(42) && self.iri(p.as_str()) && self.lit(l),
                C::DataMinCardinality(_, p, r)
                | C::DataMaxCardinality(_, p, r)
                | C::DataExactCardinality(_, p, r) => {
                    self.add(54) && self.iri(p.as_str()) && r.as_ref().is_none_or(|x| self.dr(x))
                }
            }
        }
        fn ann(&mut self, a: &Annotation) -> bool {
            if !self.add(24) || !self.iri(a.property.as_str()) {
                return false;
            }
            let ok_val = match &a.value {
                AnnotationValue::Iri(i) => self.iri(i.as_str()),
                AnnotationValue::Literal(l) => self.lit(l),
                AnnotationValue::Anonymous(b) => self.add(b.label().len().saturating_add(4)),
            };
            ok_val && a.annotations.iter().all(|inner| self.ann(inner))
        }
    }

    use AxiomKind as K;
    let mut c = Counter {
        used: 0,
        limit: max_bytes,
    };
    if !c.add(ax.axiom_type().name().len().saturating_add(12)) {
        return false;
    }
    let body_ok = match &ax.kind {
        K::Declaration(e) => {
            c.add(e.kind().name().len().saturating_add(16)) && c.iri(e.iri().as_str())
        }
        K::SubClassOf(a, b) => c.add(26) && c.ce(a) && c.ce(b),
        K::EquivalentClasses(cs) => c.add(12) && cs.iter().all(|x| c.ce(x)),
        K::DisjointClasses(cs) => c.add(12) && cs.iter().all(|x| c.ce(x)),
        K::DisjointUnion(cls, cs) => c.add(20) && c.iri(cls.as_str()) && cs.iter().all(|x| c.ce(x)),
        K::SubObjectPropertyOf(sub, sup) => {
            let sub_ok = match sub {
                SubObjectPropertyExpression::Expression(p) => c.ope(p),
                SubObjectPropertyExpression::Chain(ps) => ps.iter().all(|p| c.ope(p)),
            };
            c.add(30) && sub_ok && c.ope(sup)
        }
        K::EquivalentObjectProperties(ps) => c.add(16) && ps.iter().all(|p| c.ope(p)),
        K::DisjointObjectProperties(ps) => c.add(16) && ps.iter().all(|p| c.ope(p)),
        K::InverseObjectProperties(a, b) => c.add(20) && c.ope(a) && c.ope(b),
        K::ObjectPropertyDomain(p, ce) | K::ObjectPropertyRange(p, ce) => {
            c.add(22) && c.ope(p) && c.ce(ce)
        }
        K::FunctionalObjectProperty(p)
        | K::InverseFunctionalObjectProperty(p)
        | K::ReflexiveObjectProperty(p)
        | K::IrreflexiveObjectProperty(p)
        | K::SymmetricObjectProperty(p)
        | K::AsymmetricObjectProperty(p)
        | K::TransitiveObjectProperty(p) => c.add(13) && c.ope(p),
        K::SubDataPropertyOf(a, b) => c.add(30) && c.iri(a.as_str()) && c.iri(b.as_str()),
        K::EquivalentDataProperties(ps) => c.add(16) && ps.iter().all(|p| c.iri(p.as_str())),
        K::DisjointDataProperties(ps) => c.add(16) && ps.iter().all(|p| c.iri(p.as_str())),
        K::DataPropertyDomain(p, ce) => c.add(22) && c.iri(p.as_str()) && c.ce(ce),
        K::DataPropertyRange(p, dr) => c.add(21) && c.iri(p.as_str()) && c.dr(dr),
        K::FunctionalDataProperty(p) => c.add(13) && c.iri(p.as_str()),
        K::DatatypeDefinition(d, dr) => c.add(21) && c.iri(d.as_str()) && c.dr(dr),
        K::HasKey(ce, ops, dps) => {
            c.add(46)
                && c.ce(ce)
                && ops.iter().all(|p| c.ope(p))
                && dps.iter().all(|p| c.iri(p.as_str()))
        }
        K::SameIndividual(is) => c.add(16) && is.iter().all(|i| c.ind(i)),
        K::DifferentIndividuals(is) => c.add(16) && is.iter().all(|i| c.ind(i)),
        K::ClassAssertion(ce, i) => c.add(24) && c.ce(ce) && c.ind(i),
        K::ObjectPropertyAssertion(p, a, b) | K::NegativeObjectPropertyAssertion(p, a, b) => {
            c.add(30) && c.ope(p) && c.ind(a) && c.ind(b)
        }
        K::DataPropertyAssertion(p, a, l) | K::NegativeDataPropertyAssertion(p, a, l) => {
            c.add(30) && c.iri(p.as_str()) && c.ind(a) && c.lit(l)
        }
        K::AnnotationAssertion(p, s, v) => {
            let s_ok = match s {
                AnnotationSubject::Iri(i) => c.iri(i.as_str()),
                AnnotationSubject::Anonymous(a) => c.add(a.label().len().saturating_add(4)),
            };
            let v_ok = match v {
                AnnotationValue::Iri(i) => c.iri(i.as_str()),
                AnnotationValue::Literal(l) => c.lit(l),
                AnnotationValue::Anonymous(a) => c.add(a.label().len().saturating_add(4)),
            };
            c.add(32) && c.iri(p.as_str()) && s_ok && v_ok
        }
        K::SubAnnotationPropertyOf(a, b) => c.add(30) && c.iri(a.as_str()) && c.iri(b.as_str()),
        K::AnnotationPropertyDomain(p, i) | K::AnnotationPropertyRange(p, i) => {
            c.add(22) && c.iri(p.as_str()) && c.iri(i.as_str())
        }
    };
    if !body_ok {
        return false;
    }
    if !ax.annotations.is_empty() {
        if !c.add(16) {
            return false;
        }
        for a in &ax.annotations {
            if !c.ann(a) {
                return false;
            }
        }
    }
    true
}

/// An axiom in its JSON form.
pub fn axiom(ax: &Axiom) -> Value {
    use AxiomKind as K;
    let mut m = Map::new();
    m.insert("type".into(), ax.axiom_type().name().into());
    let mut put = |k: &str, v: Value| {
        m.insert(k.into(), v);
    };
    let ces =
        |cs: &BTreeSet<ClassExpression>| -> Value { cs.iter().map(class_expression).collect() };
    let ces_bag =
        |cs: &Multiset<ClassExpression>| -> Value { cs.iter().map(class_expression).collect() };
    let opes = |ps: &BTreeSet<ObjectPropertyExpression>| -> Value { ps.iter().map(ope).collect() };
    let opes_bag =
        |ps: &Multiset<ObjectPropertyExpression>| -> Value { ps.iter().map(ope).collect() };
    let dps = |ps: &BTreeSet<DataProperty>| -> Value { ps.iter().map(|p| iri_v(&p.0)).collect() };
    let dps_bag =
        |ps: &Multiset<DataProperty>| -> Value { ps.iter().map(|p| iri_v(&p.0)).collect() };
    match &ax.kind {
        K::Declaration(e) => {
            put("kind", e.kind().name().into());
            put("iri", iri_v(e.iri()));
        }
        K::SubClassOf(a, b) => {
            put("subClass", class_expression(a));
            put("superClass", class_expression(b));
        }
        K::EquivalentClasses(cs) => put("classes", ces(cs)),
        K::DisjointClasses(cs) => put("classes", ces_bag(cs)),
        K::DisjointUnion(c, cs) => {
            put("class", iri_v(&c.0));
            put("classes", ces_bag(cs));
        }
        K::SubObjectPropertyOf(sub, sup) => {
            match sub {
                SubObjectPropertyExpression::Expression(p) => put("subProperty", ope(p)),
                SubObjectPropertyExpression::Chain(ps) => {
                    put("chain", ps.iter().map(ope).collect())
                }
            }
            put("superProperty", ope(sup));
        }
        K::EquivalentObjectProperties(ps) => put("properties", opes(ps)),
        K::DisjointObjectProperties(ps) => put("properties", opes_bag(ps)),
        K::InverseObjectProperties(a, b) => {
            put("first", ope(a));
            put("second", ope(b));
        }
        K::ObjectPropertyDomain(p, c) => {
            put("property", ope(p));
            put("domain", class_expression(c));
        }
        K::ObjectPropertyRange(p, c) => {
            put("property", ope(p));
            put("range", class_expression(c));
        }
        K::FunctionalObjectProperty(p)
        | K::InverseFunctionalObjectProperty(p)
        | K::ReflexiveObjectProperty(p)
        | K::IrreflexiveObjectProperty(p)
        | K::SymmetricObjectProperty(p)
        | K::AsymmetricObjectProperty(p)
        | K::TransitiveObjectProperty(p) => put("property", ope(p)),
        K::SubDataPropertyOf(a, b) => {
            put("subProperty", iri_v(&a.0));
            put("superProperty", iri_v(&b.0));
        }
        K::EquivalentDataProperties(ps) => put("properties", dps(ps)),
        K::DisjointDataProperties(ps) => put("properties", dps_bag(ps)),
        K::DataPropertyDomain(p, c) => {
            put("property", iri_v(&p.0));
            put("domain", class_expression(c));
        }
        K::DataPropertyRange(p, r) => {
            put("property", iri_v(&p.0));
            put("range", data_range(r));
        }
        K::FunctionalDataProperty(p) => put("property", iri_v(&p.0)),
        K::DatatypeDefinition(d, r) => {
            put("datatype", iri_v(&d.0));
            put("range", data_range(r));
        }
        K::HasKey(c, ops, dprops) => {
            put("class", class_expression(c));
            put("objectProperties", opes(ops));
            put("dataProperties", dps(dprops));
        }
        K::SameIndividual(is) => put("individuals", is.iter().map(individual).collect()),
        K::DifferentIndividuals(is) => put("individuals", is.iter().map(individual).collect()),
        K::ClassAssertion(c, i) => {
            put("class", class_expression(c));
            put("individual", individual(i));
        }
        K::ObjectPropertyAssertion(p, a, b) | K::NegativeObjectPropertyAssertion(p, a, b) => {
            put("property", ope(p));
            put("source", individual(a));
            put("target", individual(b));
        }
        K::DataPropertyAssertion(p, a, l) | K::NegativeDataPropertyAssertion(p, a, l) => {
            put("property", iri_v(&p.0));
            put("source", individual(a));
            put("value", literal(l));
        }
        K::AnnotationAssertion(p, s, v) => {
            put("property", iri_v(&p.0));
            put(
                "subject",
                match s {
                    AnnotationSubject::Iri(i) => iri_v(i),
                    AnnotationSubject::Anonymous(a) => format!("_:{}", a.label()).into(),
                },
            );
            put("value", annotation_value(v));
        }
        K::SubAnnotationPropertyOf(a, b) => {
            put("subProperty", iri_v(&a.0));
            put("superProperty", iri_v(&b.0));
        }
        K::AnnotationPropertyDomain(p, i) => {
            put("property", iri_v(&p.0));
            put("domain", iri_v(i));
        }
        K::AnnotationPropertyRange(p, i) => {
            put("property", iri_v(&p.0));
            put("range", iri_v(i));
        }
    }
    if !ax.annotations.is_empty() {
        m.insert(
            "annotations".into(),
            ax.annotations.iter().map(annotation).collect(),
        );
    }
    Value::Object(m)
}

// ------------------------------------------------------------------ reading ------

fn obj(v: &Value) -> R<&Map<String, Value>> {
    v.as_object()
        .ok_or_else(|| ModelError(format!("expected an object, not {v}")))
}

fn field<'a>(m: &'a Map<String, Value>, k: &str) -> R<&'a Value> {
    m.get(k)
        .ok_or_else(|| ModelError(format!("missing member '{k}'")))
}

fn array<'a>(m: &'a Map<String, Value>, k: &str) -> R<&'a Vec<Value>> {
    field(m, k)?
        .as_array()
        .ok_or_else(|| ModelError(format!("'{k}' must be an array")))
}

fn read_iri(v: &Value) -> R<Iri> {
    let s = v
        .as_str()
        .ok_or_else(|| ModelError(format!("expected an IRI string, not {v}")))?;
    if s.starts_with("_:") {
        return err(format!("expected an IRI, not the blank node {s}"));
    }
    parse_iri(s)
}

pub fn read_literal(v: &Value) -> R<Literal> {
    if let Some(s) = v.as_str() {
        return Ok(Literal::simple(s));
    }
    let m = obj(v)?;
    let value = field(m, "@value")?;
    let value = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => return err("@value must be a string, a number or a boolean"),
    };
    if let Some(lang) = m.get("@language") {
        let lang = lang
            .as_str()
            .ok_or_else(|| ModelError("@language must be a string".into()))?;
        return Literal::lang(value, lang);
    }
    if let Some(t) = m.get("@type") {
        let t = read_iri(t)?;
        return Ok(Literal(oxrdf::Literal::new_typed_literal(value, t)));
    }
    match field(m, "@value")? {
        Value::Number(n) if n.is_i64() || n.is_u64() => Ok(Literal::typed(
            value,
            "http://www.w3.org/2001/XMLSchema#integer",
        )),
        Value::Number(_) => Ok(Literal::typed(
            value,
            "http://www.w3.org/2001/XMLSchema#double",
        )),
        Value::Bool(_) => Ok(Literal::typed(value, crate::vocab::XSD_BOOLEAN)),
        _ => Ok(Literal::simple(value)),
    }
}

pub fn read_individual(v: &Value) -> R<Individual> {
    let s = v
        .as_str()
        .ok_or_else(|| ModelError(format!("expected an individual, not {v}")))?;
    match s.strip_prefix("_:") {
        Some(label) if !label.is_empty() => {
            Ok(Individual::Anonymous(AnonymousIndividual::new(label)))
        }
        Some(_) => err("an anonymous individual needs a label"),
        None => Ok(Individual::Named(NamedIndividual(parse_iri(s)?))),
    }
}

pub fn read_ope(v: &Value) -> R<ObjectPropertyExpression> {
    if v.is_string() {
        return Ok(ObjectPropertyExpression::Property(ObjectProperty(
            read_iri(v)?,
        )));
    }
    let m = obj(v)?;
    Ok(ObjectPropertyExpression::InverseOf(ObjectProperty(
        read_iri(field(m, "inverseOf")?)?,
    )))
}

fn read_dp(v: &Value) -> R<DataProperty> {
    Ok(DataProperty(read_iri(v)?))
}

fn type_of(m: &Map<String, Value>) -> R<&str> {
    field(m, "type")?
        .as_str()
        .ok_or_else(|| ModelError("'type' must be a string".into()))
}

pub fn read_data_range(v: &Value) -> R<DataRange> {
    if v.is_string() {
        return Ok(DataRange::Datatype(Datatype(read_iri(v)?)));
    }
    let m = obj(v)?;
    let set =
        |k: &str| -> R<BTreeSet<DataRange>> { array(m, k)?.iter().map(read_data_range).collect() };
    Ok(match type_of(m)? {
        "Datatype" => DataRange::Datatype(Datatype(read_iri(field(m, "iri")?)?)),
        "DataIntersectionOf" => DataRange::IntersectionOf(set("ranges")?),
        "DataUnionOf" => DataRange::UnionOf(set("ranges")?),
        "DataComplementOf" => {
            DataRange::ComplementOf(Box::new(read_data_range(field(m, "range")?)?))
        }
        "DataOneOf" => DataRange::OneOf(
            array(m, "values")?
                .iter()
                .map(read_literal)
                .collect::<R<_>>()?,
        ),
        "DatatypeRestriction" => DataRange::DatatypeRestriction(
            Datatype(read_iri(field(m, "datatype")?)?),
            array(m, "restrictions")?
                .iter()
                .map(|f| {
                    let f = obj(f)?;
                    Ok(FacetRestriction {
                        facet: read_iri(field(f, "facet")?)?,
                        value: read_literal(field(f, "value")?)?,
                    })
                })
                .collect::<R<_>>()?,
        ),
        t => return err(format!("unknown data range type '{t}'")),
    })
}

fn cardinality(m: &Map<String, Value>) -> R<u32> {
    field(m, "cardinality")?
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| ModelError("'cardinality' must be a nonnegative integer".into()))
}

fn data_props(m: &Map<String, Value>) -> R<Vec<DataProperty>> {
    if let Some(p) = m.get("property") {
        return Ok(vec![read_dp(p)?]);
    }
    array(m, "properties")?.iter().map(read_dp).collect()
}

pub fn read_class_expression(v: &Value) -> R<ClassExpression> {
    use ClassExpression as C;
    if v.is_string() {
        return Ok(C::Class(Class(read_iri(v)?)));
    }
    let m = obj(v)?;
    let set = |k: &str| -> R<BTreeSet<ClassExpression>> {
        array(m, k)?.iter().map(read_class_expression).collect()
    };
    let filler = |k: &str| -> R<Option<Box<ClassExpression>>> {
        m.get(k)
            .map(|c| read_class_expression(c).map(Box::new))
            .transpose()
    };
    let range = || -> R<Option<DataRange>> { m.get("range").map(read_data_range).transpose() };
    Ok(match type_of(m)? {
        "Class" => C::Class(Class(read_iri(field(m, "iri")?)?)),
        "ObjectIntersectionOf" => C::ObjectIntersectionOf(set("classes")?),
        "ObjectUnionOf" => C::ObjectUnionOf(set("classes")?),
        "ObjectComplementOf" => {
            C::ObjectComplementOf(Box::new(read_class_expression(field(m, "class")?)?))
        }
        "ObjectOneOf" => C::ObjectOneOf(
            array(m, "individuals")?
                .iter()
                .map(read_individual)
                .collect::<R<_>>()?,
        ),
        "ObjectSomeValuesFrom" => C::ObjectSomeValuesFrom(
            read_ope(field(m, "property")?)?,
            Box::new(read_class_expression(field(m, "class")?)?),
        ),
        "ObjectAllValuesFrom" => C::ObjectAllValuesFrom(
            read_ope(field(m, "property")?)?,
            Box::new(read_class_expression(field(m, "class")?)?),
        ),
        "ObjectHasValue" => C::ObjectHasValue(
            read_ope(field(m, "property")?)?,
            read_individual(field(m, "individual")?)?,
        ),
        "ObjectHasSelf" => C::ObjectHasSelf(read_ope(field(m, "property")?)?),
        "ObjectMinCardinality" => C::ObjectMinCardinality(
            cardinality(m)?,
            read_ope(field(m, "property")?)?,
            filler("class")?,
        ),
        "ObjectMaxCardinality" => C::ObjectMaxCardinality(
            cardinality(m)?,
            read_ope(field(m, "property")?)?,
            filler("class")?,
        ),
        "ObjectExactCardinality" => C::ObjectExactCardinality(
            cardinality(m)?,
            read_ope(field(m, "property")?)?,
            filler("class")?,
        ),
        "DataSomeValuesFrom" => {
            C::DataSomeValuesFrom(data_props(m)?, read_data_range(field(m, "range")?)?)
        }
        "DataAllValuesFrom" => {
            C::DataAllValuesFrom(data_props(m)?, read_data_range(field(m, "range")?)?)
        }
        "DataHasValue" => C::DataHasValue(
            read_dp(field(m, "property")?)?,
            read_literal(field(m, "value")?)?,
        ),
        "DataMinCardinality" => {
            C::DataMinCardinality(cardinality(m)?, read_dp(field(m, "property")?)?, range()?)
        }
        "DataMaxCardinality" => {
            C::DataMaxCardinality(cardinality(m)?, read_dp(field(m, "property")?)?, range()?)
        }
        "DataExactCardinality" => {
            C::DataExactCardinality(cardinality(m)?, read_dp(field(m, "property")?)?, range()?)
        }
        t => return err(format!("unknown class expression type '{t}'")),
    })
}

fn read_annotation_value(v: &Value) -> R<AnnotationValue> {
    if let Some(s) = v.as_str() {
        if let Some(label) = s.strip_prefix("_:") {
            return Ok(AnnotationValue::Anonymous(AnonymousIndividual::new(label)));
        }
        return Ok(AnnotationValue::Iri(parse_iri(s)?));
    }
    Ok(AnnotationValue::Literal(read_literal(v)?))
}

pub fn read_annotation(v: &Value) -> R<Annotation> {
    let m = obj(v)?;
    Ok(Annotation {
        property: AnnotationProperty(read_iri(field(m, "property")?)?),
        value: read_annotation_value(field(m, "value")?)?,
        annotations: match m.get("annotations") {
            Some(a) => a
                .as_array()
                .ok_or_else(|| ModelError("'annotations' must be an array".into()))?
                .iter()
                .map(read_annotation)
                .collect::<R<_>>()?,
            None => BTreeSet::new(),
        },
    })
}

/// An axiom from its JSON form, checked with [`crate::validate::axiom`].
pub fn read_axiom(v: &Value) -> R<Axiom> {
    use AxiomKind as K;
    let m = obj(v)?;
    let t = type_of(m)?;
    let ce = |k: &str| read_class_expression(field(m, k)?);
    let ces = |k: &str| -> R<BTreeSet<ClassExpression>> {
        array(m, k)?.iter().map(read_class_expression).collect()
    };
    let ces_bag = |k: &str| -> R<Multiset<ClassExpression>> {
        array(m, k)?.iter().map(read_class_expression).collect()
    };
    let p = || read_ope(field(m, "property")?);
    let dp = || read_dp(field(m, "property")?);
    let opes = |k: &str| -> R<BTreeSet<ObjectPropertyExpression>> {
        array(m, k)?.iter().map(read_ope).collect()
    };
    let opes_bag = |k: &str| -> R<Multiset<ObjectPropertyExpression>> {
        array(m, k)?.iter().map(read_ope).collect()
    };
    let dps = |k: &str| -> R<BTreeSet<DataProperty>> { array(m, k)?.iter().map(read_dp).collect() };
    let dps_bag =
        |k: &str| -> R<Multiset<DataProperty>> { array(m, k)?.iter().map(read_dp).collect() };
    let ind = |k: &str| read_individual(field(m, k)?);
    let ap = |k: &str| Ok::<_, ModelError>(AnnotationProperty(read_iri(field(m, k)?)?));
    let kind = match t {
        "Declaration" => {
            let k = field(m, "kind")?
                .as_str()
                .and_then(EntityKind::parse)
                .ok_or_else(|| {
                    ModelError("'kind' must name an entity kind such as Class".into())
                })?;
            K::Declaration(Entity::of_kind(k, read_iri(field(m, "iri")?)?))
        }
        "SubClassOf" => K::SubClassOf(ce("subClass")?, ce("superClass")?),
        "EquivalentClasses" => K::EquivalentClasses(ces("classes")?),
        "DisjointClasses" => K::DisjointClasses(ces_bag("classes")?),
        "DisjointUnion" => {
            K::DisjointUnion(Class(read_iri(field(m, "class")?)?), ces_bag("classes")?)
        }
        "SubObjectPropertyOf" => {
            let sub = if let Some(c) = m.get("chain") {
                SubObjectPropertyExpression::Chain(
                    c.as_array()
                        .ok_or_else(|| ModelError("'chain' must be an array".into()))?
                        .iter()
                        .map(read_ope)
                        .collect::<R<_>>()?,
                )
            } else {
                SubObjectPropertyExpression::Expression(read_ope(field(m, "subProperty")?)?)
            };
            K::SubObjectPropertyOf(sub, read_ope(field(m, "superProperty")?)?)
        }
        "EquivalentObjectProperties" => K::EquivalentObjectProperties(opes("properties")?),
        "DisjointObjectProperties" => K::DisjointObjectProperties(opes_bag("properties")?),
        "InverseObjectProperties" => K::InverseObjectProperties(
            read_ope(field(m, "first")?)?,
            read_ope(field(m, "second")?)?,
        ),
        "ObjectPropertyDomain" => K::ObjectPropertyDomain(p()?, ce("domain")?),
        "ObjectPropertyRange" => K::ObjectPropertyRange(p()?, ce("range")?),
        "FunctionalObjectProperty" => K::FunctionalObjectProperty(p()?),
        "InverseFunctionalObjectProperty" => K::InverseFunctionalObjectProperty(p()?),
        "ReflexiveObjectProperty" => K::ReflexiveObjectProperty(p()?),
        "IrreflexiveObjectProperty" => K::IrreflexiveObjectProperty(p()?),
        "SymmetricObjectProperty" => K::SymmetricObjectProperty(p()?),
        "AsymmetricObjectProperty" => K::AsymmetricObjectProperty(p()?),
        "TransitiveObjectProperty" => K::TransitiveObjectProperty(p()?),
        "SubDataPropertyOf" => K::SubDataPropertyOf(
            read_dp(field(m, "subProperty")?)?,
            read_dp(field(m, "superProperty")?)?,
        ),
        "EquivalentDataProperties" => K::EquivalentDataProperties(dps("properties")?),
        "DisjointDataProperties" => K::DisjointDataProperties(dps_bag("properties")?),
        "DataPropertyDomain" => K::DataPropertyDomain(dp()?, ce("domain")?),
        "DataPropertyRange" => K::DataPropertyRange(dp()?, read_data_range(field(m, "range")?)?),
        "FunctionalDataProperty" => K::FunctionalDataProperty(dp()?),
        "DatatypeDefinition" => K::DatatypeDefinition(
            Datatype(read_iri(field(m, "datatype")?)?),
            read_data_range(field(m, "range")?)?,
        ),
        "HasKey" => K::HasKey(
            ce("class")?,
            m.get("objectProperties")
                .map_or(Ok(BTreeSet::new()), |_| opes("objectProperties"))?,
            m.get("dataProperties")
                .map_or(Ok(BTreeSet::new()), |_| dps("dataProperties"))?,
        ),
        "SameIndividual" => K::SameIndividual(
            array(m, "individuals")?
                .iter()
                .map(read_individual)
                .collect::<R<_>>()?,
        ),
        "DifferentIndividuals" => K::DifferentIndividuals(
            array(m, "individuals")?
                .iter()
                .map(read_individual)
                .collect::<R<_>>()?,
        ),
        "ClassAssertion" => K::ClassAssertion(ce("class")?, ind("individual")?),
        "ObjectPropertyAssertion" => {
            K::ObjectPropertyAssertion(p()?, ind("source")?, ind("target")?)
        }
        "NegativeObjectPropertyAssertion" => {
            K::NegativeObjectPropertyAssertion(p()?, ind("source")?, ind("target")?)
        }
        "DataPropertyAssertion" => {
            K::DataPropertyAssertion(dp()?, ind("source")?, read_literal(field(m, "value")?)?)
        }
        "NegativeDataPropertyAssertion" => K::NegativeDataPropertyAssertion(
            dp()?,
            ind("source")?,
            read_literal(field(m, "value")?)?,
        ),
        "AnnotationAssertion" => {
            let s = field(m, "subject")?;
            let subject = match s.as_str().and_then(|s| s.strip_prefix("_:")) {
                Some(l) => AnnotationSubject::Anonymous(AnonymousIndividual::new(l)),
                None => AnnotationSubject::Iri(read_iri(s)?),
            };
            K::AnnotationAssertion(
                ap("property")?,
                subject,
                read_annotation_value(field(m, "value")?)?,
            )
        }
        "SubAnnotationPropertyOf" => {
            K::SubAnnotationPropertyOf(ap("subProperty")?, ap("superProperty")?)
        }
        "AnnotationPropertyDomain" => {
            K::AnnotationPropertyDomain(ap("property")?, read_iri(field(m, "domain")?)?)
        }
        "AnnotationPropertyRange" => {
            K::AnnotationPropertyRange(ap("property")?, read_iri(field(m, "range")?)?)
        }
        t => return err(format!("unknown axiom type '{t}'")),
    };
    let annotations = match m.get("annotations") {
        Some(a) => a
            .as_array()
            .ok_or_else(|| ModelError("'annotations' must be an array".into()))?
            .iter()
            .map(read_annotation)
            .collect::<R<_>>()?,
        None => BTreeSet::new(),
    };
    let ax = Axiom { kind, annotations };
    crate::validate::axiom(&ax)?;
    Ok(ax)
}
