//! The functional-style syntax writer (structural specification §3–§10).
//!
//! [`Renderer`] writes one value with prefixed names where a prefix applies, and
//! [`document`] writes a whole ontology document with its prefix declarations. The
//! `Display` implementations of the model write with the four standard prefixes
//! (`owl:`, `rdf:`, `rdfs:`, `xsd:`) and full IRIs elsewhere.

use crate::model::*;
use crate::vocab;
use std::fmt::{self, Write};

/// A writer of functional-style syntax with a set of prefixes.
#[derive(Clone, Debug)]
pub struct Renderer {
    /// `(prefix, namespace)` pairs, longest namespace first
    prefixes: Vec<(String, String)>,
}

impl Default for Renderer {
    fn default() -> Renderer {
        Renderer::new(std::iter::empty::<(String, String)>())
    }
}

/// The prefixes every rendering knows.
pub const STANDARD_PREFIXES: [(&str, &str); 4] = [
    ("owl", vocab::OWL),
    ("rdf", vocab::RDF),
    ("rdfs", vocab::RDFS),
    ("xsd", vocab::XSD),
];

impl Renderer {
    /// A renderer with the standard prefixes and `prefixes`, which win over them for
    /// the same name.
    pub fn new<P: Into<String>, N: Into<String>>(
        prefixes: impl IntoIterator<Item = (P, N)>,
    ) -> Renderer {
        let mut v: Vec<(String, String)> = prefixes
            .into_iter()
            .map(|(p, n)| (p.into(), n.into()))
            .collect();
        for (p, n) in STANDARD_PREFIXES {
            if !v.iter().any(|(q, _)| q == p) {
                v.push((p.to_string(), n.to_string()));
            }
        }
        v.retain(|(p, n)| {
            !n.is_empty() && p.chars().all(|c| c.is_alphanumeric() || "_-.".contains(c))
        });
        v.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        Renderer { prefixes: v }
    }

    /// The prefixes, sorted by name.
    pub fn prefixes(&self) -> Vec<(&str, &str)> {
        let mut v: Vec<(&str, &str)> = self
            .prefixes
            .iter()
            .map(|(p, n)| (p.as_str(), n.as_str()))
            .collect();
        v.sort();
        v
    }

    /// Streams an IRI as a prefixed name or in angle brackets with zero intermediate allocations.
    pub(crate) fn write_iri(&self, iri: &str, w: &mut impl fmt::Write) -> fmt::Result {
        for (p, ns) in &self.prefixes {
            if let Some(local) = iri.strip_prefix(ns.as_str())
                && is_local_name(local)
            {
                w.write_str(p)?;
                w.write_char(':')?;
                return w.write_str(local);
            }
        }
        w.write_char('<')?;
        w.write_str(iri)?;
        w.write_char('>')
    }

    pub(crate) fn write_literal(&self, l: &Literal, w: &mut impl fmt::Write) -> fmt::Result {
        w.write_char('"')?;
        for c in l.value().chars() {
            match c {
                '"' => w.write_str("\\\"")?,
                '\\' => w.write_str("\\\\")?,
                c => w.write_char(c)?,
            }
        }
        w.write_char('"')?;
        if let Some(lang) = l.language() {
            w.write_char('@')?;
            w.write_str(lang)?;
        } else if l.datatype() != vocab::XSD_STRING {
            w.write_str("^^")?;
            self.write_iri(l.datatype(), w)?;
        }
        Ok(())
    }

    pub(crate) fn write_individual(&self, i: &Individual, w: &mut impl fmt::Write) -> fmt::Result {
        match i {
            Individual::Named(n) => self.write_iri(n.as_str(), w),
            Individual::Anonymous(a) => {
                w.write_str("_:")?;
                w.write_str(a.label())
            }
        }
    }

    pub(crate) fn write_ope(
        &self,
        p: &ObjectPropertyExpression,
        w: &mut impl fmt::Write,
    ) -> fmt::Result {
        match p {
            ObjectPropertyExpression::Property(p) => self.write_iri(p.as_str(), w),
            ObjectPropertyExpression::InverseOf(p) => {
                w.write_str("ObjectInverseOf(")?;
                self.write_iri(p.as_str(), w)?;
                w.write_char(')')
            }
        }
    }

    pub(crate) fn write_data_range(&self, r: &DataRange, w: &mut impl fmt::Write) -> fmt::Result {
        match r {
            DataRange::Datatype(d) => self.write_iri(d.as_str(), w),
            DataRange::IntersectionOf(rs) => {
                w.write_str("DataIntersectionOf(")?;
                for (i, r) in rs.iter().enumerate() {
                    if i > 0 {
                        w.write_char(' ')?;
                    }
                    self.write_data_range(r, w)?;
                }
                w.write_char(')')
            }
            DataRange::UnionOf(rs) => {
                w.write_str("DataUnionOf(")?;
                for (i, r) in rs.iter().enumerate() {
                    if i > 0 {
                        w.write_char(' ')?;
                    }
                    self.write_data_range(r, w)?;
                }
                w.write_char(')')
            }
            DataRange::ComplementOf(r) => {
                w.write_str("DataComplementOf(")?;
                self.write_data_range(r, w)?;
                w.write_char(')')
            }
            DataRange::OneOf(ls) => {
                w.write_str("DataOneOf(")?;
                for (i, l) in ls.iter().enumerate() {
                    if i > 0 {
                        w.write_char(' ')?;
                    }
                    self.write_literal(l, w)?;
                }
                w.write_char(')')
            }
            DataRange::DatatypeRestriction(d, fs) => {
                w.write_str("DatatypeRestriction(")?;
                self.write_iri(d.as_str(), w)?;
                for f in fs {
                    w.write_char(' ')?;
                    self.write_iri(f.facet.as_str(), w)?;
                    w.write_char(' ')?;
                    self.write_literal(&f.value, w)?;
                }
                w.write_char(')')
            }
        }
    }

    /// Streams a class expression in functional-style syntax with zero intermediate allocations.
    pub(crate) fn write_class_expression(
        &self,
        c: &ClassExpression,
        w: &mut impl fmt::Write,
    ) -> fmt::Result {
        use ClassExpression as C;
        match c {
            C::Class(c) => self.write_iri(c.as_str(), w),
            C::ObjectIntersectionOf(cs) => {
                w.write_str("ObjectIntersectionOf(")?;
                for (i, c) in cs.iter().enumerate() {
                    if i > 0 {
                        w.write_char(' ')?;
                    }
                    self.write_class_expression(c, w)?;
                }
                w.write_char(')')
            }
            C::ObjectUnionOf(cs) => {
                w.write_str("ObjectUnionOf(")?;
                for (i, c) in cs.iter().enumerate() {
                    if i > 0 {
                        w.write_char(' ')?;
                    }
                    self.write_class_expression(c, w)?;
                }
                w.write_char(')')
            }
            C::ObjectComplementOf(c) => {
                w.write_str("ObjectComplementOf(")?;
                self.write_class_expression(c, w)?;
                w.write_char(')')
            }
            C::ObjectOneOf(is) => {
                w.write_str("ObjectOneOf(")?;
                for (i, ind) in is.iter().enumerate() {
                    if i > 0 {
                        w.write_char(' ')?;
                    }
                    self.write_individual(ind, w)?;
                }
                w.write_char(')')
            }
            C::ObjectSomeValuesFrom(p, c) => {
                w.write_str("ObjectSomeValuesFrom(")?;
                self.write_ope(p, w)?;
                w.write_char(' ')?;
                self.write_class_expression(c, w)?;
                w.write_char(')')
            }
            C::ObjectAllValuesFrom(p, c) => {
                w.write_str("ObjectAllValuesFrom(")?;
                self.write_ope(p, w)?;
                w.write_char(' ')?;
                self.write_class_expression(c, w)?;
                w.write_char(')')
            }
            C::ObjectHasValue(p, i) => {
                w.write_str("ObjectHasValue(")?;
                self.write_ope(p, w)?;
                w.write_char(' ')?;
                self.write_individual(i, w)?;
                w.write_char(')')
            }
            C::ObjectHasSelf(p) => {
                w.write_str("ObjectHasSelf(")?;
                self.write_ope(p, w)?;
                w.write_char(')')
            }
            C::ObjectMinCardinality(n, p, f)
            | C::ObjectMaxCardinality(n, p, f)
            | C::ObjectExactCardinality(n, p, f) => {
                let name = match c {
                    C::ObjectMinCardinality(..) => "ObjectMinCardinality",
                    C::ObjectMaxCardinality(..) => "ObjectMaxCardinality",
                    C::ObjectExactCardinality(..) => "ObjectExactCardinality",
                    _ => unreachable!(),
                };
                write!(w, "{name}({n} ")?;
                self.write_ope(p, w)?;
                if let Some(f) = f {
                    w.write_char(' ')?;
                    self.write_class_expression(f, w)?;
                }
                w.write_char(')')
            }
            C::DataSomeValuesFrom(ps, r) | C::DataAllValuesFrom(ps, r) => {
                let name = match c {
                    C::DataSomeValuesFrom(..) => "DataSomeValuesFrom",
                    C::DataAllValuesFrom(..) => "DataAllValuesFrom",
                    _ => unreachable!(),
                };
                w.write_str(name)?;
                w.write_char('(')?;
                for p in ps {
                    self.write_iri(p.as_str(), w)?;
                    w.write_char(' ')?;
                }
                self.write_data_range(r, w)?;
                w.write_char(')')
            }
            C::DataHasValue(p, l) => {
                w.write_str("DataHasValue(")?;
                self.write_iri(p.as_str(), w)?;
                w.write_char(' ')?;
                self.write_literal(l, w)?;
                w.write_char(')')
            }
            C::DataMinCardinality(n, p, r)
            | C::DataMaxCardinality(n, p, r)
            | C::DataExactCardinality(n, p, r) => {
                let name = match c {
                    C::DataMinCardinality(..) => "DataMinCardinality",
                    C::DataMaxCardinality(..) => "DataMaxCardinality",
                    C::DataExactCardinality(..) => "DataExactCardinality",
                    _ => unreachable!(),
                };
                write!(w, "{name}({n} ")?;
                self.write_iri(p.as_str(), w)?;
                if let Some(r) = r {
                    w.write_char(' ')?;
                    self.write_data_range(r, w)?;
                }
                w.write_char(')')
            }
        }
    }

    /// An IRI as a prefixed name, or in angle brackets.
    pub fn iri(&self, iri: &str) -> String {
        for (p, ns) in &self.prefixes {
            if let Some(local) = iri.strip_prefix(ns.as_str())
                && is_local_name(local)
            {
                return format!("{p}:{local}");
            }
        }
        format!("<{iri}>")
    }

    pub fn literal(&self, l: &Literal) -> String {
        let mut s = String::from("\"");
        for c in l.value().chars() {
            match c {
                '"' => s.push_str("\\\""),
                '\\' => s.push_str("\\\\"),
                c => s.push(c),
            }
        }
        s.push('"');
        if let Some(lang) = l.language() {
            s.push('@');
            s.push_str(lang);
        } else if l.datatype() != vocab::XSD_STRING {
            s.push_str("^^");
            s.push_str(&self.iri(l.datatype()));
        }
        s
    }

    pub fn individual(&self, i: &Individual) -> String {
        match i {
            Individual::Named(n) => self.iri(n.as_str()),
            Individual::Anonymous(a) => format!("_:{}", a.label()),
        }
    }

    pub fn ope(&self, p: &ObjectPropertyExpression) -> String {
        match p {
            ObjectPropertyExpression::Property(p) => self.iri(p.as_str()),
            ObjectPropertyExpression::InverseOf(p) => {
                format!("ObjectInverseOf({})", self.iri(p.as_str()))
            }
        }
    }

    pub fn data_range(&self, r: &DataRange) -> String {
        match r {
            DataRange::Datatype(d) => self.iri(d.as_str()),
            DataRange::IntersectionOf(rs) => {
                self.call("DataIntersectionOf", rs.iter().map(|r| self.data_range(r)))
            }
            DataRange::UnionOf(rs) => {
                self.call("DataUnionOf", rs.iter().map(|r| self.data_range(r)))
            }
            DataRange::ComplementOf(r) => format!("DataComplementOf({})", self.data_range(r)),
            DataRange::OneOf(ls) => self.call("DataOneOf", ls.iter().map(|l| self.literal(l))),
            DataRange::DatatypeRestriction(d, fs) => {
                self.call(
                    "DatatypeRestriction",
                    std::iter::once(self.iri(d.as_str())).chain(fs.iter().map(|f| {
                        format!("{} {}", self.iri(f.facet.as_str()), self.literal(&f.value))
                    })),
                )
            }
        }
    }

    pub fn class_expression(&self, c: &ClassExpression) -> String {
        use ClassExpression as C;
        let card = |name: &str, n: &u32, p: String, f: Option<String>| match f {
            Some(f) => format!("{name}({n} {p} {f})"),
            None => format!("{name}({n} {p})"),
        };
        match c {
            C::Class(c) => self.iri(c.as_str()),
            C::ObjectIntersectionOf(cs) => self.call(
                "ObjectIntersectionOf",
                cs.iter().map(|c| self.class_expression(c)),
            ),
            C::ObjectUnionOf(cs) => {
                self.call("ObjectUnionOf", cs.iter().map(|c| self.class_expression(c)))
            }
            C::ObjectComplementOf(c) => format!("ObjectComplementOf({})", self.class_expression(c)),
            C::ObjectOneOf(is) => self.call("ObjectOneOf", is.iter().map(|i| self.individual(i))),
            C::ObjectSomeValuesFrom(p, c) => {
                format!(
                    "ObjectSomeValuesFrom({} {})",
                    self.ope(p),
                    self.class_expression(c)
                )
            }
            C::ObjectAllValuesFrom(p, c) => {
                format!(
                    "ObjectAllValuesFrom({} {})",
                    self.ope(p),
                    self.class_expression(c)
                )
            }
            C::ObjectHasValue(p, i) => {
                format!("ObjectHasValue({} {})", self.ope(p), self.individual(i))
            }
            C::ObjectHasSelf(p) => format!("ObjectHasSelf({})", self.ope(p)),
            C::ObjectMinCardinality(n, p, f) => card(
                "ObjectMinCardinality",
                n,
                self.ope(p),
                f.as_ref().map(|f| self.class_expression(f)),
            ),
            C::ObjectMaxCardinality(n, p, f) => card(
                "ObjectMaxCardinality",
                n,
                self.ope(p),
                f.as_ref().map(|f| self.class_expression(f)),
            ),
            C::ObjectExactCardinality(n, p, f) => card(
                "ObjectExactCardinality",
                n,
                self.ope(p),
                f.as_ref().map(|f| self.class_expression(f)),
            ),
            C::DataSomeValuesFrom(ps, r) => self.call(
                "DataSomeValuesFrom",
                ps.iter()
                    .map(|p| self.iri(p.as_str()))
                    .chain(std::iter::once(self.data_range(r))),
            ),
            C::DataAllValuesFrom(ps, r) => self.call(
                "DataAllValuesFrom",
                ps.iter()
                    .map(|p| self.iri(p.as_str()))
                    .chain(std::iter::once(self.data_range(r))),
            ),
            C::DataHasValue(p, l) => {
                format!("DataHasValue({} {})", self.iri(p.as_str()), self.literal(l))
            }
            C::DataMinCardinality(n, p, r) => card(
                "DataMinCardinality",
                n,
                self.iri(p.as_str()),
                r.as_ref().map(|r| self.data_range(r)),
            ),
            C::DataMaxCardinality(n, p, r) => card(
                "DataMaxCardinality",
                n,
                self.iri(p.as_str()),
                r.as_ref().map(|r| self.data_range(r)),
            ),
            C::DataExactCardinality(n, p, r) => card(
                "DataExactCardinality",
                n,
                self.iri(p.as_str()),
                r.as_ref().map(|r| self.data_range(r)),
            ),
        }
    }

    pub fn entity(&self, e: &Entity) -> String {
        format!("{}({})", e.kind().name(), self.iri(e.iri().as_str()))
    }

    pub fn annotation_value(&self, v: &AnnotationValue) -> String {
        match v {
            AnnotationValue::Iri(i) => self.iri(i.as_str()),
            AnnotationValue::Literal(l) => self.literal(l),
            AnnotationValue::Anonymous(a) => format!("_:{}", a.label()),
        }
    }

    pub fn annotation(&self, a: &Annotation) -> String {
        let mut parts: Vec<String> = a.annotations.iter().map(|a| self.annotation(a)).collect();
        parts.push(self.iri(a.property.as_str()));
        parts.push(self.annotation_value(&a.value));
        format!("Annotation({})", parts.join(" "))
    }

    /// One axiom, with its annotations first as the syntax places them.
    pub fn axiom(&self, ax: &Axiom) -> String {
        use AxiomKind as K;
        let mut args: Vec<String> = ax.annotations.iter().map(|a| self.annotation(a)).collect();
        let ces = |cs: &std::collections::BTreeSet<ClassExpression>| -> Vec<String> {
            cs.iter().map(|c| self.class_expression(c)).collect()
        };
        let opes = |ps: &std::collections::BTreeSet<ObjectPropertyExpression>| -> Vec<String> {
            ps.iter().map(|p| self.ope(p)).collect()
        };
        let dps = |ps: &std::collections::BTreeSet<DataProperty>| -> Vec<String> {
            ps.iter().map(|p| self.iri(p.as_str())).collect()
        };
        let inds = |is: &std::collections::BTreeSet<Individual>| -> Vec<String> {
            is.iter().map(|i| self.individual(i)).collect()
        };
        match &ax.kind {
            K::Declaration(e) => args.push(self.entity(e)),
            K::SubClassOf(a, b) => {
                args.push(self.class_expression(a));
                args.push(self.class_expression(b));
            }
            K::EquivalentClasses(cs) | K::DisjointClasses(cs) => args.extend(ces(cs)),
            K::DisjointUnion(c, cs) => {
                args.push(self.iri(c.as_str()));
                args.extend(ces(cs));
            }
            K::SubObjectPropertyOf(sub, sup) => {
                args.push(match sub {
                    SubObjectPropertyExpression::Expression(p) => self.ope(p),
                    SubObjectPropertyExpression::Chain(ps) => {
                        self.call("ObjectPropertyChain", ps.iter().map(|p| self.ope(p)))
                    }
                });
                args.push(self.ope(sup));
            }
            K::EquivalentObjectProperties(ps) | K::DisjointObjectProperties(ps) => {
                args.extend(opes(ps))
            }
            K::InverseObjectProperties(a, b) => {
                args.push(self.ope(a));
                args.push(self.ope(b));
            }
            K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => {
                args.push(self.ope(p));
                args.push(self.class_expression(c));
            }
            K::FunctionalObjectProperty(p)
            | K::InverseFunctionalObjectProperty(p)
            | K::ReflexiveObjectProperty(p)
            | K::IrreflexiveObjectProperty(p)
            | K::SymmetricObjectProperty(p)
            | K::AsymmetricObjectProperty(p)
            | K::TransitiveObjectProperty(p) => args.push(self.ope(p)),
            K::SubDataPropertyOf(a, b) => {
                args.push(self.iri(a.as_str()));
                args.push(self.iri(b.as_str()));
            }
            K::EquivalentDataProperties(ps) | K::DisjointDataProperties(ps) => args.extend(dps(ps)),
            K::DataPropertyDomain(p, c) => {
                args.push(self.iri(p.as_str()));
                args.push(self.class_expression(c));
            }
            K::DataPropertyRange(p, r) => {
                args.push(self.iri(p.as_str()));
                args.push(self.data_range(r));
            }
            K::FunctionalDataProperty(p) => args.push(self.iri(p.as_str())),
            K::DatatypeDefinition(d, r) => {
                args.push(self.iri(d.as_str()));
                args.push(self.data_range(r));
            }
            K::HasKey(c, ops, dprops) => {
                args.push(self.class_expression(c));
                args.push(format!("({})", opes(ops).join(" ")));
                args.push(format!("({})", dps(dprops).join(" ")));
            }
            K::SameIndividual(is) | K::DifferentIndividuals(is) => args.extend(inds(is)),
            K::ClassAssertion(c, i) => {
                args.push(self.class_expression(c));
                args.push(self.individual(i));
            }
            K::ObjectPropertyAssertion(p, a, b) | K::NegativeObjectPropertyAssertion(p, a, b) => {
                args.push(self.ope(p));
                args.push(self.individual(a));
                args.push(self.individual(b));
            }
            K::DataPropertyAssertion(p, a, l) | K::NegativeDataPropertyAssertion(p, a, l) => {
                args.push(self.iri(p.as_str()));
                args.push(self.individual(a));
                args.push(self.literal(l));
            }
            K::AnnotationAssertion(p, s, v) => {
                args.push(self.iri(p.as_str()));
                args.push(match s {
                    AnnotationSubject::Iri(i) => self.iri(i.as_str()),
                    AnnotationSubject::Anonymous(a) => format!("_:{}", a.label()),
                });
                args.push(self.annotation_value(v));
            }
            K::SubAnnotationPropertyOf(a, b) => {
                args.push(self.iri(a.as_str()));
                args.push(self.iri(b.as_str()));
            }
            K::AnnotationPropertyDomain(p, i) | K::AnnotationPropertyRange(p, i) => {
                args.push(self.iri(p.as_str()));
                args.push(self.iri(i.as_str()));
            }
        }
        format!("{}({})", ax.axiom_type().name(), args.join(" "))
    }

    fn call(&self, name: &str, args: impl Iterator<Item = String>) -> String {
        let args: Vec<String> = args.collect();
        format!("{name}({})", args.join(" "))
    }
}

/// Whether `s` can follow a prefix in a prefixed name: a simplified PN_LOCAL that
/// accepts letters, digits, `_` and `-`, and `.` inside.
fn is_local_name(s: &str) -> bool {
    !s.is_empty()
        && !s.ends_with('.')
        && !s.starts_with(['-', '.'])
        && s.chars().all(|c| c.is_alphanumeric() || "_-.".contains(c))
}

/// An ontology document in functional-style syntax: the prefix declarations, the
/// header and the axioms in their order.
pub fn document<'a>(
    r: &Renderer,
    header: &Header,
    axioms: impl IntoIterator<Item = &'a Axiom>,
) -> String {
    let mut s = String::new();
    for (p, n) in r.prefixes() {
        let _ = writeln!(s, "Prefix({p}:=<{n}>)");
    }
    s.push('\n');
    s.push_str("Ontology(");
    if let Some(i) = &header.iri {
        let _ = write!(s, "<{}>", i.as_str());
        if let Some(v) = &header.version_iri {
            let _ = write!(s, " <{}>", v.as_str());
        }
    }
    s.push('\n');
    for i in &header.imports {
        let _ = writeln!(s, "Import(<{}>)", i.as_str());
    }
    for a in &header.annotations {
        let _ = writeln!(s, "{}", r.annotation(a));
    }
    s.push('\n');
    for ax in axioms {
        let _ = writeln!(s, "{}", r.axiom(ax));
    }
    s.push_str(")\n");
    s
}

thread_local! {
    static DEFAULT: Renderer = Renderer::default();
}

macro_rules! display {
    ($t:ty, $m:ident) => {
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                DEFAULT.with(|r| f.write_str(&r.$m(self)))
            }
        }
    };
}

display!(Axiom, axiom);
impl fmt::Display for ClassExpression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        DEFAULT.with(|r| r.write_class_expression(self, f))
    }
}
display!(DataRange, data_range);
display!(ObjectPropertyExpression, ope);
display!(Individual, individual);
display!(Literal, literal);
display!(Annotation, annotation);
display!(Entity, entity);
