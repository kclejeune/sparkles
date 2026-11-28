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

const POLL_CHUNK_BYTES: usize = 1024;

/// Poll-aware streaming sink for functional-style syntax formatting.
pub(crate) trait PolledWrite<E> {
    fn push_str(&mut self, s: &str) -> Result<(), E>;
    fn tick(&mut self, work: usize) -> Result<(), E>;

    fn write_str_polled(&mut self, mut s: &str) -> Result<(), E> {
        if s.is_empty() {
            return self.tick(1);
        }
        while !s.is_empty() {
            let split = if s.len() <= POLL_CHUNK_BYTES {
                s.len()
            } else {
                let mut i = POLL_CHUNK_BYTES;
                while i > 0 && !s.is_char_boundary(i) {
                    i -= 1;
                }
                if i == 0 {
                    s.chars().next().map_or(s.len(), char::len_utf8)
                } else {
                    i
                }
            };
            let (head, tail) = s.split_at(split);
            self.tick(head.len())?;
            self.push_str(head)?;
            s = tail;
        }
        Ok(())
    }

    fn write_char_polled(&mut self, c: char) -> Result<(), E> {
        let mut buf = [0u8; 4];
        let s = c.encode_utf8(&mut buf);
        self.tick(1)?;
        self.push_str(s)
    }

    fn write_u32_polled(&mut self, mut n: u32) -> Result<(), E> {
        let mut buf = [0u8; 10];
        let mut i = buf.len();
        loop {
            i -= 1;
            buf[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        let s = std::str::from_utf8(&buf[i..]).expect("ASCII digits");
        self.write_str_polled(s)
    }
}

struct FmtSink<'a, W: fmt::Write + ?Sized>(&'a mut W);

impl<W: fmt::Write + ?Sized> PolledWrite<fmt::Error> for FmtSink<'_, W> {
    fn push_str(&mut self, s: &str) -> fmt::Result {
        self.0.write_str(s)
    }

    fn tick(&mut self, _work: usize) -> fmt::Result {
        Ok(())
    }
}

struct BoundedSink {
    buf: String,
    limit: usize,
}

/// A sink that hands each piece's size to the caller's `tick` before it writes it.
struct ControlledSink<'a, E> {
    buf: String,
    tick: &'a mut dyn FnMut(usize) -> Result<(), E>,
}

impl<E> PolledWrite<E> for ControlledSink<'_, E> {
    fn push_str(&mut self, s: &str) -> Result<(), E> {
        self.buf.push_str(s);
        Ok(())
    }

    fn tick(&mut self, work: usize) -> Result<(), E> {
        (self.tick)(work)
    }
}

impl PolledWrite<()> for BoundedSink {
    fn push_str(&mut self, s: &str) -> Result<(), ()> {
        if self.buf.len().saturating_add(s.len()) > self.limit {
            return Err(());
        }
        self.buf.push_str(s);
        Ok(())
    }

    fn tick(&mut self, _work: usize) -> Result<(), ()> {
        Ok(())
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
        let mut by_name: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (p, n) in prefixes {
            let p = p.into();
            let n = n.into();
            if !n.is_empty() && (p.is_empty() || crate::write_common::is_pn_prefix(&p)) {
                by_name.insert(p, n);
            }
        }
        for (p, n) in STANDARD_PREFIXES {
            by_name
                .entry(p.to_string())
                .or_insert_with(|| n.to_string());
        }
        let mut v: Vec<(String, String)> = by_name.into_iter().collect();
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

    /// Streams an IRI as a prefixed name or in angle brackets with cooperative polling.
    pub(crate) fn write_iri_polled<E>(
        &self,
        iri: &str,
        w: &mut impl PolledWrite<E>,
    ) -> Result<(), E> {
        for (p, ns) in &self.prefixes {
            if let Some(local) = iri.strip_prefix(ns.as_str())
                && is_local_name_polled(local, w)?
            {
                w.write_str_polled(p)?;
                w.write_char_polled(':')?;
                return w.write_str_polled(local);
            }
        }
        w.write_char_polled('<')?;
        w.write_str_polled(iri)?;
        w.write_char_polled('>')
    }

    pub(crate) fn write_literal_polled<E>(
        &self,
        l: &Literal,
        w: &mut impl PolledWrite<E>,
    ) -> Result<(), E> {
        w.write_char_polled('"')?;
        for c in l.value().chars() {
            match c {
                '"' => w.write_str_polled("\\\"")?,
                '\\' => w.write_str_polled("\\\\")?,
                c => w.write_char_polled(c)?,
            }
        }
        w.write_char_polled('"')?;
        if let Some(lang) = l.language() {
            w.write_char_polled('@')?;
            w.write_str_polled(lang)?;
        } else if l.datatype() != vocab::XSD_STRING {
            w.write_str_polled("^^")?;
            self.write_iri_polled(l.datatype(), w)?;
        }
        Ok(())
    }

    pub(crate) fn write_individual_polled<E>(
        &self,
        i: &Individual,
        w: &mut impl PolledWrite<E>,
    ) -> Result<(), E> {
        match i {
            Individual::Named(n) => self.write_iri_polled(n.as_str(), w),
            Individual::Anonymous(a) => {
                w.write_str_polled("_:")?;
                w.write_str_polled(a.label())
            }
        }
    }

    pub(crate) fn write_ope_polled<E>(
        &self,
        p: &ObjectPropertyExpression,
        w: &mut impl PolledWrite<E>,
    ) -> Result<(), E> {
        match p {
            ObjectPropertyExpression::Property(p) => self.write_iri_polled(p.as_str(), w),
            ObjectPropertyExpression::InverseOf(p) => {
                w.write_str_polled("ObjectInverseOf(")?;
                self.write_iri_polled(p.as_str(), w)?;
                w.write_char_polled(')')
            }
        }
    }

    pub(crate) fn write_data_range_polled<E>(
        &self,
        r: &DataRange,
        w: &mut impl PolledWrite<E>,
    ) -> Result<(), E> {
        match r {
            DataRange::Datatype(d) => self.write_iri_polled(d.as_str(), w),
            DataRange::IntersectionOf(rs) => {
                w.write_str_polled("DataIntersectionOf(")?;
                for (i, r) in rs.iter().enumerate() {
                    if i > 0 {
                        w.write_char_polled(' ')?;
                    }
                    self.write_data_range_polled(r, w)?;
                }
                w.write_char_polled(')')
            }
            DataRange::UnionOf(rs) => {
                w.write_str_polled("DataUnionOf(")?;
                for (i, r) in rs.iter().enumerate() {
                    if i > 0 {
                        w.write_char_polled(' ')?;
                    }
                    self.write_data_range_polled(r, w)?;
                }
                w.write_char_polled(')')
            }
            DataRange::ComplementOf(r) => {
                w.write_str_polled("DataComplementOf(")?;
                self.write_data_range_polled(r, w)?;
                w.write_char_polled(')')
            }
            DataRange::OneOf(ls) => {
                w.write_str_polled("DataOneOf(")?;
                for (i, l) in ls.iter().enumerate() {
                    if i > 0 {
                        w.write_char_polled(' ')?;
                    }
                    self.write_literal_polled(l, w)?;
                }
                w.write_char_polled(')')
            }
            DataRange::DatatypeRestriction(d, fs) => {
                w.write_str_polled("DatatypeRestriction(")?;
                self.write_iri_polled(d.as_str(), w)?;
                for f in fs {
                    w.write_char_polled(' ')?;
                    self.write_iri_polled(f.facet.as_str(), w)?;
                    w.write_char_polled(' ')?;
                    self.write_literal_polled(&f.value, w)?;
                }
                w.write_char_polled(')')
            }
        }
    }

    /// Streams a class expression in functional-style syntax with cooperative polling
    /// and zero intermediate allocations.
    pub(crate) fn write_class_expression_polled<E>(
        &self,
        c: &ClassExpression,
        w: &mut impl PolledWrite<E>,
    ) -> Result<(), E> {
        use ClassExpression as C;
        match c {
            C::Class(c) => self.write_iri_polled(c.as_str(), w),
            C::ObjectIntersectionOf(cs) => {
                w.write_str_polled("ObjectIntersectionOf(")?;
                for (i, c) in cs.iter().enumerate() {
                    if i > 0 {
                        w.write_char_polled(' ')?;
                    }
                    self.write_class_expression_polled(c, w)?;
                }
                w.write_char_polled(')')
            }
            C::ObjectUnionOf(cs) => {
                w.write_str_polled("ObjectUnionOf(")?;
                for (i, c) in cs.iter().enumerate() {
                    if i > 0 {
                        w.write_char_polled(' ')?;
                    }
                    self.write_class_expression_polled(c, w)?;
                }
                w.write_char_polled(')')
            }
            C::ObjectComplementOf(c) => {
                w.write_str_polled("ObjectComplementOf(")?;
                self.write_class_expression_polled(c, w)?;
                w.write_char_polled(')')
            }
            C::ObjectOneOf(is) => {
                w.write_str_polled("ObjectOneOf(")?;
                for (i, ind) in is.iter().enumerate() {
                    if i > 0 {
                        w.write_char_polled(' ')?;
                    }
                    self.write_individual_polled(ind, w)?;
                }
                w.write_char_polled(')')
            }
            C::ObjectSomeValuesFrom(p, c) => {
                w.write_str_polled("ObjectSomeValuesFrom(")?;
                self.write_ope_polled(p, w)?;
                w.write_char_polled(' ')?;
                self.write_class_expression_polled(c, w)?;
                w.write_char_polled(')')
            }
            C::ObjectAllValuesFrom(p, c) => {
                w.write_str_polled("ObjectAllValuesFrom(")?;
                self.write_ope_polled(p, w)?;
                w.write_char_polled(' ')?;
                self.write_class_expression_polled(c, w)?;
                w.write_char_polled(')')
            }
            C::ObjectHasValue(p, i) => {
                w.write_str_polled("ObjectHasValue(")?;
                self.write_ope_polled(p, w)?;
                w.write_char_polled(' ')?;
                self.write_individual_polled(i, w)?;
                w.write_char_polled(')')
            }
            C::ObjectHasSelf(p) => {
                w.write_str_polled("ObjectHasSelf(")?;
                self.write_ope_polled(p, w)?;
                w.write_char_polled(')')
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
                w.write_str_polled(name)?;
                w.write_char_polled('(')?;
                w.write_u32_polled(*n)?;
                w.write_char_polled(' ')?;
                self.write_ope_polled(p, w)?;
                if let Some(f) = f {
                    w.write_char_polled(' ')?;
                    self.write_class_expression_polled(f, w)?;
                }
                w.write_char_polled(')')
            }
            C::DataSomeValuesFrom(ps, r) | C::DataAllValuesFrom(ps, r) => {
                let name = match c {
                    C::DataSomeValuesFrom(..) => "DataSomeValuesFrom",
                    C::DataAllValuesFrom(..) => "DataAllValuesFrom",
                    _ => unreachable!(),
                };
                w.write_str_polled(name)?;
                w.write_char_polled('(')?;
                for p in ps {
                    self.write_iri_polled(p.as_str(), w)?;
                    w.write_char_polled(' ')?;
                }
                self.write_data_range_polled(r, w)?;
                w.write_char_polled(')')
            }
            C::DataHasValue(p, l) => {
                w.write_str_polled("DataHasValue(")?;
                self.write_iri_polled(p.as_str(), w)?;
                w.write_char_polled(' ')?;
                self.write_literal_polled(l, w)?;
                w.write_char_polled(')')
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
                w.write_str_polled(name)?;
                w.write_char_polled('(')?;
                w.write_u32_polled(*n)?;
                w.write_char_polled(' ')?;
                self.write_iri_polled(p.as_str(), w)?;
                if let Some(r) = r {
                    w.write_char_polled(' ')?;
                    self.write_data_range_polled(r, w)?;
                }
                w.write_char_polled(')')
            }
        }
    }

    /// Streams a class expression in functional-style syntax with zero intermediate allocations.
    pub(crate) fn write_class_expression(
        &self,
        c: &ClassExpression,
        w: &mut impl fmt::Write,
    ) -> fmt::Result {
        self.write_class_expression_polled(c, &mut FmtSink(w))
    }

    pub(crate) fn write_entity_polled<E>(
        &self,
        e: &Entity,
        w: &mut impl PolledWrite<E>,
    ) -> Result<(), E> {
        w.write_str_polled(e.kind().name())?;
        w.write_char_polled('(')?;
        self.write_iri_polled(e.iri().as_str(), w)?;
        w.write_char_polled(')')
    }

    pub(crate) fn write_annotation_value_polled<E>(
        &self,
        v: &AnnotationValue,
        w: &mut impl PolledWrite<E>,
    ) -> Result<(), E> {
        match v {
            AnnotationValue::Iri(i) => self.write_iri_polled(i.as_str(), w),
            AnnotationValue::Literal(l) => self.write_literal_polled(l, w),
            AnnotationValue::Anonymous(a) => {
                w.write_str_polled("_:")?;
                w.write_str_polled(a.label())
            }
        }
    }

    pub(crate) fn write_annotation_polled<E>(
        &self,
        a: &Annotation,
        w: &mut impl PolledWrite<E>,
    ) -> Result<(), E> {
        w.write_str_polled("Annotation(")?;
        for inner in &a.annotations {
            self.write_annotation_polled(inner, w)?;
            w.write_char_polled(' ')?;
        }
        self.write_iri_polled(a.property.as_str(), w)?;
        w.write_char_polled(' ')?;
        self.write_annotation_value_polled(&a.value, w)?;
        w.write_char_polled(')')
    }

    pub(crate) fn write_axiom_polled<E>(
        &self,
        ax: &Axiom,
        w: &mut impl PolledWrite<E>,
    ) -> Result<(), E> {
        use AxiomKind as K;
        w.write_str_polled(ax.axiom_type().name())?;
        w.write_char_polled('(')?;
        let mut first = true;
        let mut sep = |w: &mut dyn FnMut() -> Result<(), E>| -> Result<(), E> {
            if first {
                first = false;
                Ok(())
            } else {
                w()
            }
        };
        for a in &ax.annotations {
            sep(&mut || w.write_char_polled(' '))?;
            self.write_annotation_polled(a, w)?;
        }
        match &ax.kind {
            K::Declaration(e) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_entity_polled(e, w)?;
            }
            K::SubClassOf(a, b) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_class_expression_polled(a, w)?;
                w.write_char_polled(' ')?;
                self.write_class_expression_polled(b, w)?;
            }
            K::EquivalentClasses(cs) => {
                for c in cs {
                    sep(&mut || w.write_char_polled(' '))?;
                    self.write_class_expression_polled(c, w)?;
                }
            }
            K::DisjointClasses(cs) => {
                for c in cs {
                    sep(&mut || w.write_char_polled(' '))?;
                    self.write_class_expression_polled(c, w)?;
                }
            }
            K::DisjointUnion(c, cs) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_iri_polled(c.as_str(), w)?;
                for x in cs {
                    w.write_char_polled(' ')?;
                    self.write_class_expression_polled(x, w)?;
                }
            }
            K::SubObjectPropertyOf(sub, sup) => {
                sep(&mut || w.write_char_polled(' '))?;
                match sub {
                    SubObjectPropertyExpression::Expression(p) => self.write_ope_polled(p, w)?,
                    SubObjectPropertyExpression::Chain(ps) => {
                        w.write_str_polled("ObjectPropertyChain(")?;
                        for (i, p) in ps.iter().enumerate() {
                            if i > 0 {
                                w.write_char_polled(' ')?;
                            }
                            self.write_ope_polled(p, w)?;
                        }
                        w.write_char_polled(')')?;
                    }
                }
                w.write_char_polled(' ')?;
                self.write_ope_polled(sup, w)?;
            }
            K::EquivalentObjectProperties(ps) => {
                for p in ps {
                    sep(&mut || w.write_char_polled(' '))?;
                    self.write_ope_polled(p, w)?;
                }
            }
            K::DisjointObjectProperties(ps) => {
                for p in ps {
                    sep(&mut || w.write_char_polled(' '))?;
                    self.write_ope_polled(p, w)?;
                }
            }
            K::InverseObjectProperties(a, b) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_ope_polled(a, w)?;
                w.write_char_polled(' ')?;
                self.write_ope_polled(b, w)?;
            }
            K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_ope_polled(p, w)?;
                w.write_char_polled(' ')?;
                self.write_class_expression_polled(c, w)?;
            }
            K::FunctionalObjectProperty(p)
            | K::InverseFunctionalObjectProperty(p)
            | K::ReflexiveObjectProperty(p)
            | K::IrreflexiveObjectProperty(p)
            | K::SymmetricObjectProperty(p)
            | K::AsymmetricObjectProperty(p)
            | K::TransitiveObjectProperty(p) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_ope_polled(p, w)?;
            }
            K::SubDataPropertyOf(a, b) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_iri_polled(a.as_str(), w)?;
                w.write_char_polled(' ')?;
                self.write_iri_polled(b.as_str(), w)?;
            }
            K::EquivalentDataProperties(ps) => {
                for p in ps {
                    sep(&mut || w.write_char_polled(' '))?;
                    self.write_iri_polled(p.as_str(), w)?;
                }
            }
            K::DisjointDataProperties(ps) => {
                for p in ps {
                    sep(&mut || w.write_char_polled(' '))?;
                    self.write_iri_polled(p.as_str(), w)?;
                }
            }
            K::DataPropertyDomain(p, c) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_iri_polled(p.as_str(), w)?;
                w.write_char_polled(' ')?;
                self.write_class_expression_polled(c, w)?;
            }
            K::DataPropertyRange(p, r) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_iri_polled(p.as_str(), w)?;
                w.write_char_polled(' ')?;
                self.write_data_range_polled(r, w)?;
            }
            K::FunctionalDataProperty(p) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_iri_polled(p.as_str(), w)?;
            }
            K::DatatypeDefinition(d, r) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_iri_polled(d.as_str(), w)?;
                w.write_char_polled(' ')?;
                self.write_data_range_polled(r, w)?;
            }
            K::HasKey(c, ops, dprops) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_class_expression_polled(c, w)?;
                w.write_str_polled(" (")?;
                for (i, p) in ops.iter().enumerate() {
                    if i > 0 {
                        w.write_char_polled(' ')?;
                    }
                    self.write_ope_polled(p, w)?;
                }
                w.write_str_polled(") (")?;
                for (i, p) in dprops.iter().enumerate() {
                    if i > 0 {
                        w.write_char_polled(' ')?;
                    }
                    self.write_iri_polled(p.as_str(), w)?;
                }
                w.write_char_polled(')')?;
            }
            K::SameIndividual(is) => {
                for i in is {
                    sep(&mut || w.write_char_polled(' '))?;
                    self.write_individual_polled(i, w)?;
                }
            }
            K::DifferentIndividuals(is) => {
                for i in is {
                    sep(&mut || w.write_char_polled(' '))?;
                    self.write_individual_polled(i, w)?;
                }
            }
            K::ClassAssertion(c, i) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_class_expression_polled(c, w)?;
                w.write_char_polled(' ')?;
                self.write_individual_polled(i, w)?;
            }
            K::ObjectPropertyAssertion(p, a, b) | K::NegativeObjectPropertyAssertion(p, a, b) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_ope_polled(p, w)?;
                w.write_char_polled(' ')?;
                self.write_individual_polled(a, w)?;
                w.write_char_polled(' ')?;
                self.write_individual_polled(b, w)?;
            }
            K::DataPropertyAssertion(p, a, l) | K::NegativeDataPropertyAssertion(p, a, l) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_iri_polled(p.as_str(), w)?;
                w.write_char_polled(' ')?;
                self.write_individual_polled(a, w)?;
                w.write_char_polled(' ')?;
                self.write_literal_polled(l, w)?;
            }
            K::AnnotationAssertion(p, s, v) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_iri_polled(p.as_str(), w)?;
                w.write_char_polled(' ')?;
                match s {
                    AnnotationSubject::Iri(i) => self.write_iri_polled(i.as_str(), w)?,
                    AnnotationSubject::Anonymous(a) => {
                        w.write_str_polled("_:")?;
                        w.write_str_polled(a.label())?;
                    }
                }
                w.write_char_polled(' ')?;
                self.write_annotation_value_polled(v, w)?;
            }
            K::SubAnnotationPropertyOf(a, b) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_iri_polled(a.as_str(), w)?;
                w.write_char_polled(' ')?;
                self.write_iri_polled(b.as_str(), w)?;
            }
            K::AnnotationPropertyDomain(p, i) | K::AnnotationPropertyRange(p, i) => {
                sep(&mut || w.write_char_polled(' '))?;
                self.write_iri_polled(p.as_str(), w)?;
                w.write_char_polled(' ')?;
                self.write_iri_polled(i.as_str(), w)?;
            }
        }
        w.write_char_polled(')')
    }

    /// Render `e` into functional-style syntax if it fits within `max_bytes`, returning
    /// `None` immediately upon exceeding `max_bytes`.
    pub fn entity_bounded(&self, e: &Entity, max_bytes: usize) -> Option<String> {
        let mut sink = BoundedSink {
            buf: String::new(),
            limit: max_bytes,
        };
        self.write_entity_polled(e, &mut sink).ok()?;
        Some(sink.buf)
    }

    /// Render `ax` into functional-style syntax if it fits within `max_bytes`, returning
    /// `None` immediately upon exceeding `max_bytes`.
    pub fn axiom_bounded(&self, ax: &Axiom, max_bytes: usize) -> Option<String> {
        let mut sink = BoundedSink {
            buf: String::new(),
            limit: max_bytes,
        };
        self.write_axiom_polled(ax, &mut sink).ok()?;
        Some(sink.buf)
    }

    /// [`axiom`](Self::axiom) with `tick` called with the size of each piece of the
    /// text before it is written, so that the caller can stop the rendering of a large
    /// axiom, such as an equivalence of many classes, with its own error. The text is
    /// the same as `axiom`'s.
    pub fn axiom_controlled<E>(
        &self,
        ax: &Axiom,
        tick: &mut dyn FnMut(usize) -> Result<(), E>,
    ) -> Result<String, E> {
        let mut sink = ControlledSink {
            buf: String::new(),
            tick,
        };
        self.write_axiom_polled(ax, &mut sink)?;
        Ok(sink.buf)
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
        let ces_bag = |cs: &Multiset<ClassExpression>| -> Vec<String> {
            cs.iter().map(|c| self.class_expression(c)).collect()
        };
        let opes = |ps: &std::collections::BTreeSet<ObjectPropertyExpression>| -> Vec<String> {
            ps.iter().map(|p| self.ope(p)).collect()
        };
        let opes_bag = |ps: &Multiset<ObjectPropertyExpression>| -> Vec<String> {
            ps.iter().map(|p| self.ope(p)).collect()
        };
        let dps = |ps: &std::collections::BTreeSet<DataProperty>| -> Vec<String> {
            ps.iter().map(|p| self.iri(p.as_str())).collect()
        };
        let dps_bag = |ps: &Multiset<DataProperty>| -> Vec<String> {
            ps.iter().map(|p| self.iri(p.as_str())).collect()
        };
        let inds = |is: &std::collections::BTreeSet<Individual>| -> Vec<String> {
            is.iter().map(|i| self.individual(i)).collect()
        };
        let inds_bag = |is: &Multiset<Individual>| -> Vec<String> {
            is.iter().map(|i| self.individual(i)).collect()
        };
        match &ax.kind {
            K::Declaration(e) => args.push(self.entity(e)),
            K::SubClassOf(a, b) => {
                args.push(self.class_expression(a));
                args.push(self.class_expression(b));
            }
            K::EquivalentClasses(cs) => args.extend(ces(cs)),
            K::DisjointClasses(cs) => args.extend(ces_bag(cs)),
            K::DisjointUnion(c, cs) => {
                args.push(self.iri(c.as_str()));
                args.extend(ces_bag(cs));
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
            K::EquivalentObjectProperties(ps) => args.extend(opes(ps)),
            K::DisjointObjectProperties(ps) => args.extend(opes_bag(ps)),
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
            K::EquivalentDataProperties(ps) => args.extend(dps(ps)),
            K::DisjointDataProperties(ps) => args.extend(dps_bag(ps)),
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
            K::SameIndividual(is) => args.extend(inds(is)),
            K::DifferentIndividuals(is) => args.extend(inds_bag(is)),
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

/// Whether `s` can follow a prefix in a prefixed name: a simplified `PN_LOCAL`
/// matching W3C `PN_CHARS_U | [0-9]` followed by `PN_CHARS | '.'` not ending in `.`.
fn is_local_name(s: &str) -> bool {
    crate::write_common::is_pn_local(s)
}

fn is_local_name_polled<E>(s: &str, w: &mut impl PolledWrite<E>) -> Result<bool, E> {
    let mut cs = s.chars();
    let Some(first) = cs.next() else {
        return Ok(false);
    };
    if !(crate::write_common::is_pn_chars_u(first) || first.is_ascii_digit()) {
        return Ok(false);
    }
    let mut last = first;
    let mut batch = 1usize;
    for c in cs {
        batch += 1;
        if batch == POLL_CHUNK_BYTES {
            w.tick(batch)?;
            batch = 0;
        }
        if !(crate::write_common::is_pn_chars(c) || c == '.') {
            if batch > 0 {
                w.tick(batch)?;
            }
            return Ok(false);
        }
        last = c;
    }
    if batch > 0 {
        w.tick(batch)?;
    }
    Ok(last != '.')
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

pub(crate) fn write_default_class_expression_polled<E>(
    c: &ClassExpression,
    w: &mut impl PolledWrite<E>,
) -> Result<(), E> {
    DEFAULT.with(|r| r.write_class_expression_polled(c, w))
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

#[cfg(test)]
mod controlled_tests {
    use super::*;

    #[test]
    fn a_controlled_rendering_matches_the_plain_one_and_stops_inside_a_large_axiom() {
        let r = Renderer::new([("ex", "http://ex.org/")]);
        let class = |i: usize| ClassExpression::Class(Class::new(format!("http://ex.org/C{i}")));
        let eq = Axiom::equivalent_classes((0..2000).map(class));
        let sub = Axiom::sub_class_of(
            Class::new("http://ex.org/A"),
            Class::new("http://other.org/B"),
        );
        for ax in [&eq, &sub] {
            let mut ticks = 0usize;
            let text = r
                .axiom_controlled(ax, &mut |_| -> Result<(), ()> {
                    ticks += 1;
                    Ok(())
                })
                .unwrap();
            assert_eq!(text, r.axiom(ax));
            assert!(ticks > 0);
        }
        // a stop partway through the equivalence ends the rendering there
        let mut ticks = 0usize;
        let stopped = r.axiom_controlled(&eq, &mut |_| {
            ticks += 1;
            if ticks == 100 { Err("stop") } else { Ok(()) }
        });
        assert_eq!(stopped, Err("stop"));
        assert_eq!(ticks, 100);
    }
}
