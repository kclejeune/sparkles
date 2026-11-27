//! The Manchester syntax writer.

use super::lex::{ends_word, number_like};
use super::parse::{AXIOM_KEYWORDS, CHARACTERISTICS, KEYWORDS};
use super::{Found, Vocabulary, lex};
use crate::model::*;
use crate::vocab;

type Ce = ClassExpression;
type Ope = ObjectPropertyExpression;

/// A name as a bare word when the reader would read it back as one, and otherwise in
/// single quotes with `'` and `\` escaped: a name with a space, a bracket, a quote or a
/// colon, one written as a number, a keyword, or one that starts as an anonymous
/// individual does.
pub fn quote_name(s: &str) -> String {
    let bare = !s.is_empty()
        && !s.contains(ends_word)
        && !s.contains([':', '^', '@'])
        && !number_like(s)
        && !KEYWORDS.contains(&s)
        && !AXIOM_KEYWORDS.contains(&s)
        && !CHARACTERISTICS.contains(&s)
        && s != "o";
    if bare {
        return s.to_string();
    }
    let mut q = String::from("'");
    for c in s.chars() {
        if c == '\'' || c == '\\' {
            q.push('\\');
        }
        q.push(c);
    }
    q.push('\'');
    q
}

/// A writer of the Manchester syntax. An entity is written by its label when the
/// renderer has a [`Vocabulary`] in which that label names it alone among the entities
/// of its kind, and otherwise by its prefixed name or its IRI.
#[derive(Clone, Debug)]
pub struct Renderer<'a> {
    iris: crate::ofn::Renderer,
    vocabulary: Option<&'a Vocabulary>,
}

/// Error returned by [`Renderer::axiom_bounded`] when rendering would exceed `max_bytes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderBudgetExceeded;

impl<'a> Renderer<'a> {
    /// A renderer that writes prefixed names with `prefixes` and the standard ones.
    pub fn new<P: Into<String>, N: Into<String>>(
        prefixes: impl IntoIterator<Item = (P, N)>,
    ) -> Renderer<'a> {
        Renderer {
            iris: crate::ofn::Renderer::new(prefixes),
            vocabulary: None,
        }
    }

    /// A renderer with the prefixes of `v` that writes the labels of `v`.
    pub fn of_vocabulary(v: &'a Vocabulary) -> Renderer<'a> {
        Renderer {
            iris: crate::ofn::Renderer::new(v.prefixes().iter().cloned()),
            vocabulary: Some(v),
        }
    }

    /// The name of an entity.
    pub fn name(&self, e: &Entity) -> String {
        let iri = e.iri().as_str();
        if let Some(v) = self.vocabulary
            && let Some(label) = v.label(iri)
            // the label names this IRI alone among every kind, since a position such as
            // a restriction's property reads more than one kind
            && let Found::Entities(es) =
                v.lookup(&lex::Name::Quoted(label.to_string()), &EntityKind::ALL)
            && es.iter().all(|x| x.iri().as_str() == iri)
            && es.iter().any(|x| x.kind() == e.kind())
        {
            return quote_name(label);
        }
        self.iris.iri(iri)
    }

    fn class(&self, c: &Class) -> String {
        self.name(&Entity::Class(c.clone()))
    }

    fn datatype(&self, d: &Datatype) -> String {
        self.name(&Entity::Datatype(d.clone()))
    }

    fn object_property(&self, p: &ObjectProperty) -> String {
        self.name(&Entity::ObjectProperty(p.clone()))
    }

    fn data_property(&self, p: &DataProperty) -> String {
        self.name(&Entity::DataProperty(p.clone()))
    }

    fn annotation_property(&self, p: &AnnotationProperty) -> String {
        self.name(&Entity::AnnotationProperty(p.clone()))
    }

    /// An object property expression: a property or `inverse p`.
    pub fn ope(&self, p: &Ope) -> String {
        match p {
            Ope::Property(p) => self.object_property(p),
            Ope::InverseOf(p) => format!("inverse {}", self.object_property(p)),
        }
    }

    pub fn individual(&self, i: &Individual) -> String {
        match i {
            Individual::Named(n) => self.name(&Entity::NamedIndividual(n.clone())),
            Individual::Anonymous(a) => format!("_:{}", a.label()),
        }
    }

    /// A literal: a W3C Manchester quoted string (escaping `"` and `\`) with its language
    /// tag or datatype, or a bare integer or decimal when its lexical form reads back as
    /// one.
    pub fn literal(&self, l: &Literal) -> String {
        let v = quote_manchester_literal(l.value());
        if let Some(lang) = l.language() {
            return format!("{v}@{lang}");
        }
        let dt = l.datatype();
        if dt == vocab::XSD_STRING {
            return v;
        }
        if bare_literal(l.value(), dt) {
            return l.value().to_string();
        }
        format!("{v}^^{}", self.datatype(&Datatype::new(dt)))
    }

    /// A class expression, with nested n-ary expressions and restrictions in
    /// parentheses.
    pub fn class_expression(&self, c: &Ce) -> String {
        self.ce(c, false)
    }

    fn ce(&self, c: &Ce, nested: bool) -> String {
        let wrap = |s: String| if nested { format!("({s})") } else { s };
        let m = |c: &Ce| self.ce(c, true);
        let card = |p: String, word: &str, n: &u32, filler: Option<String>| {
            wrap(match filler {
                Some(f) => format!("{p} {word} {n} {f}"),
                None => format!("{p} {word} {n}"),
            })
        };
        match c {
            Ce::Class(c) => self.class(c),
            Ce::ObjectIntersectionOf(cs) => {
                wrap(cs.iter().map(m).collect::<Vec<_>>().join(" and "))
            }
            Ce::ObjectUnionOf(cs) => wrap(cs.iter().map(m).collect::<Vec<_>>().join(" or ")),
            Ce::ObjectComplementOf(c) => match c.as_ref() {
                Ce::ObjectComplementOf(_) => format!("not ({})", self.ce(c, false)),
                _ => format!("not {}", m(c)),
            },
            Ce::ObjectOneOf(is) => format!(
                "{{{}}}",
                is.iter()
                    .map(|i| self.individual(i))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Ce::ObjectSomeValuesFrom(p, f) => wrap(format!("{} some {}", self.ope(p), m(f))),
            Ce::ObjectAllValuesFrom(p, f) => wrap(format!("{} only {}", self.ope(p), m(f))),
            Ce::ObjectHasValue(p, i) => {
                wrap(format!("{} value {}", self.ope(p), self.individual(i)))
            }
            Ce::ObjectHasSelf(p) => wrap(format!("{} Self", self.ope(p))),
            Ce::ObjectMinCardinality(n, p, f) => card(self.ope(p), "min", n, f.as_deref().map(m)),
            Ce::ObjectMaxCardinality(n, p, f) => card(self.ope(p), "max", n, f.as_deref().map(m)),
            Ce::ObjectExactCardinality(n, p, f) => {
                card(self.ope(p), "exactly", n, f.as_deref().map(m))
            }
            Ce::DataSomeValuesFrom(ps, r) => wrap(format!(
                "{} some {}",
                self.data_properties(ps),
                self.dr(r, true)
            )),
            Ce::DataAllValuesFrom(ps, r) => wrap(format!(
                "{} only {}",
                self.data_properties(ps),
                self.dr(r, true)
            )),
            Ce::DataHasValue(p, l) => wrap(format!(
                "{} value {}",
                self.data_property(p),
                self.literal(l)
            )),
            Ce::DataMinCardinality(n, p, r) => card(
                self.data_property(p),
                "min",
                n,
                r.as_ref().map(|r| self.dr(r, true)),
            ),
            Ce::DataMaxCardinality(n, p, r) => card(
                self.data_property(p),
                "max",
                n,
                r.as_ref().map(|r| self.dr(r, true)),
            ),
            Ce::DataExactCardinality(n, p, r) => card(
                self.data_property(p),
                "exactly",
                n,
                r.as_ref().map(|r| self.dr(r, true)),
            ),
        }
    }

    /// The properties of a data restriction. The Manchester syntax has no n-ary data
    /// restrictions, so only the first of several is written, and the text does not
    /// read back as the expression.
    fn data_properties(&self, ps: &[DataProperty]) -> String {
        ps.first()
            .map(|p| self.data_property(p))
            .unwrap_or_default()
    }

    pub fn data_range(&self, r: &DataRange) -> String {
        self.dr(r, false)
    }

    fn dr(&self, r: &DataRange, nested: bool) -> String {
        let wrap = |s: String| if nested { format!("({s})") } else { s };
        match r {
            DataRange::Datatype(d) => self.datatype(d),
            DataRange::IntersectionOf(rs) => wrap(
                rs.iter()
                    .map(|r| self.dr(r, true))
                    .collect::<Vec<_>>()
                    .join(" and "),
            ),
            DataRange::UnionOf(rs) => wrap(
                rs.iter()
                    .map(|r| self.dr(r, true))
                    .collect::<Vec<_>>()
                    .join(" or "),
            ),
            DataRange::ComplementOf(r) => match r.as_ref() {
                DataRange::ComplementOf(_) => format!("not ({})", self.dr(r, false)),
                _ => format!("not {}", self.dr(r, true)),
            },
            DataRange::OneOf(ls) => format!(
                "{{{}}}",
                ls.iter()
                    .map(|l| self.literal(l))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            DataRange::DatatypeRestriction(d, fs) => format!(
                "{}[{}]",
                self.datatype(d),
                fs.iter()
                    .map(|f| format!(
                        "{} {}",
                        facet_word(f.facet.as_str()),
                        self.literal(&f.value)
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// An axiom in the form [`super::axiom`] reads, or `None` for an axiom that has no
    /// such form, such as a declaration, an annotation assertion, a key, a disjoint
    /// union or an n-ary axiom of more than two operands.
    pub fn axiom(&self, ax: &Axiom) -> Option<String> {
        use AxiomKind as A;
        let two = |n: usize| n == 2;
        Some(match &ax.kind {
            A::SubClassOf(a, b) => format!(
                "{} SubClassOf {}",
                self.class_expression(a),
                self.class_expression(b)
            ),
            A::EquivalentClasses(cs) if two(cs.len()) => {
                let mut it = cs.iter();
                format!(
                    "{} EquivalentTo {}",
                    self.class_expression(it.next()?),
                    self.class_expression(it.next()?)
                )
            }
            A::DisjointClasses(cs) if two(cs.len()) => {
                let mut it = cs.iter();
                format!(
                    "{} DisjointWith {}",
                    self.class_expression(it.next()?),
                    self.class_expression(it.next()?)
                )
            }
            A::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(a), b) => {
                format!("{} SubPropertyOf {}", self.ope(a), self.ope(b))
            }
            A::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(ps), b) => format!(
                "{} SubPropertyOf {}",
                ps.iter()
                    .map(|p| self.ope(p))
                    .collect::<Vec<_>>()
                    .join(" o "),
                self.ope(b)
            ),
            A::EquivalentObjectProperties(ps) if two(ps.len()) => {
                let mut it = ps.iter();
                format!(
                    "{} EquivalentTo {}",
                    self.ope(it.next()?),
                    self.ope(it.next()?)
                )
            }
            A::DisjointObjectProperties(ps) if two(ps.len()) => {
                let mut it = ps.iter();
                format!(
                    "{} DisjointWith {}",
                    self.ope(it.next()?),
                    self.ope(it.next()?)
                )
            }
            A::InverseObjectProperties(a, b) => {
                format!("{} InverseOf {}", self.ope(a), self.ope(b))
            }
            A::ObjectPropertyDomain(p, c) => {
                format!("{} Domain {}", self.ope(p), self.class_expression(c))
            }
            A::ObjectPropertyRange(p, c) => {
                format!("{} Range {}", self.ope(p), self.class_expression(c))
            }
            A::FunctionalObjectProperty(p) => format!("Functional {}", self.ope(p)),
            A::InverseFunctionalObjectProperty(p) => {
                format!("InverseFunctional {}", self.ope(p))
            }
            A::ReflexiveObjectProperty(p) => format!("Reflexive {}", self.ope(p)),
            A::IrreflexiveObjectProperty(p) => format!("Irreflexive {}", self.ope(p)),
            A::SymmetricObjectProperty(p) => format!("Symmetric {}", self.ope(p)),
            A::AsymmetricObjectProperty(p) => format!("Asymmetric {}", self.ope(p)),
            A::TransitiveObjectProperty(p) => format!("Transitive {}", self.ope(p)),
            A::SubDataPropertyOf(a, b) => format!(
                "{} SubPropertyOf {}",
                self.data_property(a),
                self.data_property(b)
            ),
            A::EquivalentDataProperties(ps) if two(ps.len()) => {
                let mut it = ps.iter();
                format!(
                    "{} EquivalentTo {}",
                    self.data_property(it.next()?),
                    self.data_property(it.next()?)
                )
            }
            A::DisjointDataProperties(ps) if two(ps.len()) => {
                let mut it = ps.iter();
                format!(
                    "{} DisjointWith {}",
                    self.data_property(it.next()?),
                    self.data_property(it.next()?)
                )
            }
            A::DataPropertyDomain(p, c) => format!(
                "{} Domain {}",
                self.data_property(p),
                self.class_expression(c)
            ),
            A::DataPropertyRange(p, r) => {
                format!("{} Range {}", self.data_property(p), self.data_range(r))
            }
            A::FunctionalDataProperty(p) => format!("Functional {}", self.data_property(p)),
            A::SameIndividual(is) if two(is.len()) => {
                let mut it = is.iter();
                format!(
                    "{} SameAs {}",
                    self.individual(it.next()?),
                    self.individual(it.next()?)
                )
            }
            A::DifferentIndividuals(is) if two(is.len()) => {
                let mut it = is.iter();
                format!(
                    "{} DifferentFrom {}",
                    self.individual(it.next()?),
                    self.individual(it.next()?)
                )
            }
            A::ClassAssertion(c, i) => {
                format!("{} Type {}", self.individual(i), self.class_expression(c))
            }
            A::ObjectPropertyAssertion(p, a, b) => format!(
                "{} {} {}",
                self.individual(a),
                self.ope(p),
                self.individual(b)
            ),
            A::NegativeObjectPropertyAssertion(p, a, b) => format!(
                "{} not {} {}",
                self.individual(a),
                self.ope(p),
                self.individual(b)
            ),
            A::DataPropertyAssertion(p, a, l) => format!(
                "{} {} {}",
                self.individual(a),
                self.data_property(p),
                self.literal(l)
            ),
            A::NegativeDataPropertyAssertion(p, a, l) => format!(
                "{} not {} {}",
                self.individual(a),
                self.data_property(p),
                self.literal(l)
            ),
            A::SubAnnotationPropertyOf(a, b) => format!(
                "{} SubPropertyOf {}",
                self.annotation_property(a),
                self.annotation_property(b)
            ),
            A::AnnotationPropertyDomain(p, i) => format!(
                "{} Domain {}",
                self.annotation_property(p),
                self.iris.iri(i.as_str())
            ),
            A::AnnotationPropertyRange(p, i) => format!(
                "{} Range {}",
                self.annotation_property(p),
                self.iris.iri(i.as_str())
            ),
            _ => return None,
        })
    }

    /// Render `ax` in Manchester syntax if it has one and fits within `max_bytes`.
    /// Returns `Ok(None)` when the axiom has no Manchester form, `Ok(Some(s))` when it
    /// fits within `max_bytes`, and `Err(RenderBudgetExceeded)` immediately upon exceeding
    /// `max_bytes` without allocating unbounded intermediate strings (even for very long labels).
    pub fn axiom_bounded(
        &self,
        ax: &Axiom,
        max_bytes: usize,
    ) -> Result<Option<String>, RenderBudgetExceeded> {
        let mut out = BoundedBuf::new(max_bytes);
        let has_form = self
            .write_axiom_bounded(ax, &mut out)
            .map_err(|()| RenderBudgetExceeded)?;
        Ok(has_form.then_some(out.buf))
    }

    fn write_quote_name_bounded(s: &str, out: &mut BoundedBuf) -> Result<(), ()> {
        let bare = !s.is_empty()
            && !s.contains(ends_word)
            && !s.contains([':', '^', '@'])
            && !number_like(s)
            && !KEYWORDS.contains(&s)
            && !AXIOM_KEYWORDS.contains(&s)
            && !CHARACTERISTICS.contains(&s)
            && s != "o";
        if bare {
            return out.push_str(s);
        }
        out.push_char('\'')?;
        for c in s.chars() {
            if c == '\'' || c == '\\' {
                out.push_char('\\')?;
            }
            out.push_char(c)?;
        }
        out.push_char('\'')
    }

    fn write_iri_bounded(&self, iri: &str, out: &mut BoundedBuf) -> Result<(), ()> {
        let rem = out.remaining();
        let Some(rendered) = self
            .iris
            .entity_bounded(&Entity::Class(Class::new(iri)), rem.saturating_add(8))
        else {
            return Err(());
        };
        // `entity_bounded` formats `Class(<iri_or_curie>)`
        let inner = rendered
            .strip_prefix("Class(")
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(&rendered);
        out.push_str(inner)
    }

    fn write_name_bounded(&self, e: &Entity, out: &mut BoundedBuf) -> Result<(), ()> {
        let iri = e.iri().as_str();
        if let Some(v) = self.vocabulary
            && let Some(label) = v.label(iri)
            && v.label_unique_for(iri, label, e.kind())
        {
            return Self::write_quote_name_bounded(label, out);
        }
        self.write_iri_bounded(iri, out)
    }

    fn write_ope_bounded(&self, p: &Ope, out: &mut BoundedBuf) -> Result<(), ()> {
        match p {
            Ope::Property(p) => self.write_name_bounded(&Entity::ObjectProperty(p.clone()), out),
            Ope::InverseOf(p) => {
                out.push_str("inverse ")?;
                self.write_name_bounded(&Entity::ObjectProperty(p.clone()), out)
            }
        }
    }

    fn write_individual_bounded(&self, i: &Individual, out: &mut BoundedBuf) -> Result<(), ()> {
        match i {
            Individual::Named(n) => {
                self.write_name_bounded(&Entity::NamedIndividual(n.clone()), out)
            }
            Individual::Anonymous(a) => {
                out.push_str("_:")?;
                out.push_str(a.label())
            }
        }
    }

    fn write_json_quoted_bounded(s: &str, out: &mut BoundedBuf) -> Result<(), ()> {
        out.push_char('"')?;
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\"")?,
                '\\' => out.push_str("\\\\")?,
                '\u{08}' => out.push_str("\\b")?,
                '\u{0C}' => out.push_str("\\f")?,
                '\n' => out.push_str("\\n")?,
                '\r' => out.push_str("\\r")?,
                '\t' => out.push_str("\\t")?,
                c if (c as u32) < 0x20 => {
                    let mut buf = [0u8; 6];
                    let hex = b"0123456789abcdef";
                    let v = c as u32;
                    buf[0] = b'\\';
                    buf[1] = b'u';
                    buf[2] = b'0';
                    buf[3] = b'0';
                    buf[4] = hex[((v >> 4) & 0xf) as usize];
                    buf[5] = hex[(v & 0xf) as usize];
                    out.push_str(std::str::from_utf8(&buf).expect("ascii"))?;
                }
                c => out.push_char(c)?,
            }
        }
        out.push_char('"')
    }

    fn write_literal_bounded(&self, l: &Literal, out: &mut BoundedBuf) -> Result<(), ()> {
        if let Some(lang) = l.language() {
            Self::write_json_quoted_bounded(l.value(), out)?;
            out.push_char('@')?;
            return out.push_str(lang);
        }
        let dt = l.datatype();
        if dt == vocab::XSD_STRING {
            return Self::write_json_quoted_bounded(l.value(), out);
        }
        if bare_literal(l.value(), dt) {
            return out.push_str(l.value());
        }
        Self::write_json_quoted_bounded(l.value(), out)?;
        out.push_str("^^")?;
        self.write_name_bounded(&Entity::Datatype(Datatype::new(dt)), out)
    }

    fn write_dr_bounded(
        &self,
        r: &DataRange,
        nested: bool,
        out: &mut BoundedBuf,
    ) -> Result<(), ()> {
        match r {
            DataRange::Datatype(d) => self.write_name_bounded(&Entity::Datatype(d.clone()), out),
            DataRange::IntersectionOf(rs) | DataRange::UnionOf(rs) => {
                if nested {
                    out.push_char('(')?;
                }
                let joiner = if matches!(r, DataRange::IntersectionOf(_)) {
                    " and "
                } else {
                    " or "
                };
                for (i, inner) in rs.iter().enumerate() {
                    if i > 0 {
                        out.push_str(joiner)?;
                    }
                    self.write_dr_bounded(inner, true, out)?;
                }
                if nested {
                    out.push_char(')')?;
                }
                Ok(())
            }
            DataRange::ComplementOf(inner) => {
                out.push_str("not ")?;
                self.write_dr_bounded(inner, true, out)
            }
            DataRange::OneOf(ls) => {
                out.push_char('{')?;
                for (i, l) in ls.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ")?;
                    }
                    self.write_literal_bounded(l, out)?;
                }
                out.push_char('}')
            }
            DataRange::DatatypeRestriction(d, fs) => {
                self.write_name_bounded(&Entity::Datatype(d.clone()), out)?;
                out.push_char('[')?;
                for (i, f) in fs.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ")?;
                    }
                    out.push_str(facet_word(f.facet.as_str()))?;
                    out.push_char(' ')?;
                    self.write_literal_bounded(&f.value, out)?;
                }
                out.push_char(']')
            }
        }
    }

    fn write_ce_bounded(&self, c: &Ce, nested: bool, out: &mut BoundedBuf) -> Result<(), ()> {
        match c {
            Ce::Class(c) => self.write_name_bounded(&Entity::Class(c.clone()), out),
            Ce::ObjectIntersectionOf(cs) | Ce::ObjectUnionOf(cs) => {
                if nested {
                    out.push_char('(')?;
                }
                let joiner = if matches!(c, Ce::ObjectIntersectionOf(_)) {
                    " and "
                } else {
                    " or "
                };
                for (i, inner) in cs.iter().enumerate() {
                    if i > 0 {
                        out.push_str(joiner)?;
                    }
                    self.write_ce_bounded(inner, true, out)?;
                }
                if nested {
                    out.push_char(')')?;
                }
                Ok(())
            }
            Ce::ObjectComplementOf(inner) => {
                out.push_str("not ")?;
                self.write_ce_bounded(inner, true, out)
            }
            Ce::ObjectOneOf(is) => {
                out.push_char('{')?;
                for (i, ind) in is.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ")?;
                    }
                    self.write_individual_bounded(ind, out)?;
                }
                out.push_char('}')
            }
            Ce::ObjectSomeValuesFrom(p, f) | Ce::ObjectAllValuesFrom(p, f) => {
                if nested {
                    out.push_char('(')?;
                }
                self.write_ope_bounded(p, out)?;
                out.push_str(if matches!(c, Ce::ObjectSomeValuesFrom(..)) {
                    " some "
                } else {
                    " only "
                })?;
                self.write_ce_bounded(f, true, out)?;
                if nested {
                    out.push_char(')')?;
                }
                Ok(())
            }
            Ce::ObjectHasValue(p, i) => {
                if nested {
                    out.push_char('(')?;
                }
                self.write_ope_bounded(p, out)?;
                out.push_str(" value ")?;
                self.write_individual_bounded(i, out)?;
                if nested {
                    out.push_char(')')?;
                }
                Ok(())
            }
            Ce::ObjectHasSelf(p) => {
                if nested {
                    out.push_char('(')?;
                }
                self.write_ope_bounded(p, out)?;
                out.push_str(" Self")?;
                if nested {
                    out.push_char(')')?;
                }
                Ok(())
            }
            Ce::ObjectMinCardinality(n, p, f)
            | Ce::ObjectMaxCardinality(n, p, f)
            | Ce::ObjectExactCardinality(n, p, f) => {
                let word = match c {
                    Ce::ObjectMinCardinality(..) => " min ",
                    Ce::ObjectMaxCardinality(..) => " max ",
                    Ce::ObjectExactCardinality(..) => " exactly ",
                    _ => unreachable!(),
                };
                if nested {
                    out.push_char('(')?;
                }
                self.write_ope_bounded(p, out)?;
                out.push_str(word)?;
                out.push_u32(*n)?;
                if let Some(f) = f {
                    out.push_char(' ')?;
                    self.write_ce_bounded(f, true, out)?;
                }
                if nested {
                    out.push_char(')')?;
                }
                Ok(())
            }
            Ce::DataSomeValuesFrom(ps, r) | Ce::DataAllValuesFrom(ps, r) => {
                if nested {
                    out.push_char('(')?;
                }
                if let Some(p) = ps.first() {
                    self.write_name_bounded(&Entity::DataProperty(p.clone()), out)?;
                }
                out.push_str(if matches!(c, Ce::DataSomeValuesFrom(..)) {
                    " some "
                } else {
                    " only "
                })?;
                self.write_dr_bounded(r, true, out)?;
                if nested {
                    out.push_char(')')?;
                }
                Ok(())
            }
            Ce::DataHasValue(p, l) => {
                if nested {
                    out.push_char('(')?;
                }
                self.write_name_bounded(&Entity::DataProperty(p.clone()), out)?;
                out.push_str(" value ")?;
                self.write_literal_bounded(l, out)?;
                if nested {
                    out.push_char(')')?;
                }
                Ok(())
            }
            Ce::DataMinCardinality(n, p, r)
            | Ce::DataMaxCardinality(n, p, r)
            | Ce::DataExactCardinality(n, p, r) => {
                let word = match c {
                    Ce::DataMinCardinality(..) => " min ",
                    Ce::DataMaxCardinality(..) => " max ",
                    Ce::DataExactCardinality(..) => " exactly ",
                    _ => unreachable!(),
                };
                if nested {
                    out.push_char('(')?;
                }
                self.write_name_bounded(&Entity::DataProperty(p.clone()), out)?;
                out.push_str(word)?;
                out.push_u32(*n)?;
                if let Some(r) = r {
                    out.push_char(' ')?;
                    self.write_dr_bounded(r, true, out)?;
                }
                if nested {
                    out.push_char(')')?;
                }
                Ok(())
            }
        }
    }

    fn write_axiom_bounded(&self, ax: &Axiom, out: &mut BoundedBuf) -> Result<bool, ()> {
        use AxiomKind as A;
        let two = |n: usize| n == 2;
        match &ax.kind {
            A::SubClassOf(a, b) => {
                self.write_ce_bounded(a, false, out)?;
                out.push_str(" SubClassOf ")?;
                self.write_ce_bounded(b, false, out)?;
            }
            A::EquivalentClasses(cs) if two(cs.len()) => {
                let mut it = cs.iter();
                self.write_ce_bounded(it.next().unwrap(), false, out)?;
                out.push_str(" EquivalentTo ")?;
                self.write_ce_bounded(it.next().unwrap(), false, out)?;
            }
            A::DisjointClasses(cs) if two(cs.len()) => {
                let mut it = cs.iter();
                self.write_ce_bounded(it.next().unwrap(), false, out)?;
                out.push_str(" DisjointWith ")?;
                self.write_ce_bounded(it.next().unwrap(), false, out)?;
            }
            A::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(a), b) => {
                self.write_ope_bounded(a, out)?;
                out.push_str(" SubPropertyOf ")?;
                self.write_ope_bounded(b, out)?;
            }
            A::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(ps), b) => {
                for (i, p) in ps.iter().enumerate() {
                    if i > 0 {
                        out.push_str(" o ")?;
                    }
                    self.write_ope_bounded(p, out)?;
                }
                out.push_str(" SubPropertyOf ")?;
                self.write_ope_bounded(b, out)?;
            }
            A::EquivalentObjectProperties(ps) if two(ps.len()) => {
                let mut it = ps.iter();
                self.write_ope_bounded(it.next().unwrap(), out)?;
                out.push_str(" EquivalentTo ")?;
                self.write_ope_bounded(it.next().unwrap(), out)?;
            }
            A::DisjointObjectProperties(ps) if two(ps.len()) => {
                let mut it = ps.iter();
                self.write_ope_bounded(it.next().unwrap(), out)?;
                out.push_str(" DisjointWith ")?;
                self.write_ope_bounded(it.next().unwrap(), out)?;
            }
            A::InverseObjectProperties(a, b) => {
                self.write_ope_bounded(a, out)?;
                out.push_str(" InverseOf ")?;
                self.write_ope_bounded(b, out)?;
            }
            A::ObjectPropertyDomain(p, c) => {
                self.write_ope_bounded(p, out)?;
                out.push_str(" Domain ")?;
                self.write_ce_bounded(c, false, out)?;
            }
            A::ObjectPropertyRange(p, c) => {
                self.write_ope_bounded(p, out)?;
                out.push_str(" Range ")?;
                self.write_ce_bounded(c, false, out)?;
            }
            A::FunctionalObjectProperty(p) => {
                out.push_str("Functional ")?;
                self.write_ope_bounded(p, out)?;
            }
            A::InverseFunctionalObjectProperty(p) => {
                out.push_str("InverseFunctional ")?;
                self.write_ope_bounded(p, out)?;
            }
            A::ReflexiveObjectProperty(p) => {
                out.push_str("Reflexive ")?;
                self.write_ope_bounded(p, out)?;
            }
            A::IrreflexiveObjectProperty(p) => {
                out.push_str("Irreflexive ")?;
                self.write_ope_bounded(p, out)?;
            }
            A::SymmetricObjectProperty(p) => {
                out.push_str("Symmetric ")?;
                self.write_ope_bounded(p, out)?;
            }
            A::AsymmetricObjectProperty(p) => {
                out.push_str("Asymmetric ")?;
                self.write_ope_bounded(p, out)?;
            }
            A::TransitiveObjectProperty(p) => {
                out.push_str("Transitive ")?;
                self.write_ope_bounded(p, out)?;
            }
            A::SubDataPropertyOf(a, b) => {
                self.write_name_bounded(&Entity::DataProperty(a.clone()), out)?;
                out.push_str(" SubPropertyOf ")?;
                self.write_name_bounded(&Entity::DataProperty(b.clone()), out)?;
            }
            A::EquivalentDataProperties(ps) if two(ps.len()) => {
                let mut it = ps.iter();
                self.write_name_bounded(&Entity::DataProperty(it.next().unwrap().clone()), out)?;
                out.push_str(" EquivalentTo ")?;
                self.write_name_bounded(&Entity::DataProperty(it.next().unwrap().clone()), out)?;
            }
            A::DisjointDataProperties(ps) if two(ps.len()) => {
                let mut it = ps.iter();
                self.write_name_bounded(&Entity::DataProperty(it.next().unwrap().clone()), out)?;
                out.push_str(" DisjointWith ")?;
                self.write_name_bounded(&Entity::DataProperty(it.next().unwrap().clone()), out)?;
            }
            A::DataPropertyDomain(p, c) => {
                self.write_name_bounded(&Entity::DataProperty(p.clone()), out)?;
                out.push_str(" Domain ")?;
                self.write_ce_bounded(c, false, out)?;
            }
            A::DataPropertyRange(p, r) => {
                self.write_name_bounded(&Entity::DataProperty(p.clone()), out)?;
                out.push_str(" Range ")?;
                self.write_dr_bounded(r, false, out)?;
            }
            A::FunctionalDataProperty(p) => {
                out.push_str("Functional ")?;
                self.write_name_bounded(&Entity::DataProperty(p.clone()), out)?;
            }
            A::SameIndividual(is) if two(is.len()) => {
                let mut it = is.iter();
                self.write_individual_bounded(it.next().unwrap(), out)?;
                out.push_str(" SameAs ")?;
                self.write_individual_bounded(it.next().unwrap(), out)?;
            }
            A::DifferentIndividuals(is) if two(is.len()) => {
                let mut it = is.iter();
                self.write_individual_bounded(it.next().unwrap(), out)?;
                out.push_str(" DifferentFrom ")?;
                self.write_individual_bounded(it.next().unwrap(), out)?;
            }
            A::ClassAssertion(c, i) => {
                self.write_individual_bounded(i, out)?;
                out.push_str(" Type ")?;
                self.write_ce_bounded(c, false, out)?;
            }
            A::ObjectPropertyAssertion(p, a, b) => {
                self.write_individual_bounded(a, out)?;
                out.push_char(' ')?;
                self.write_ope_bounded(p, out)?;
                out.push_char(' ')?;
                self.write_individual_bounded(b, out)?;
            }
            A::NegativeObjectPropertyAssertion(p, a, b) => {
                self.write_individual_bounded(a, out)?;
                out.push_str(" not ")?;
                self.write_ope_bounded(p, out)?;
                out.push_char(' ')?;
                self.write_individual_bounded(b, out)?;
            }
            A::DataPropertyAssertion(p, a, l) => {
                self.write_individual_bounded(a, out)?;
                out.push_char(' ')?;
                self.write_name_bounded(&Entity::DataProperty(p.clone()), out)?;
                out.push_char(' ')?;
                self.write_literal_bounded(l, out)?;
            }
            A::NegativeDataPropertyAssertion(p, a, l) => {
                self.write_individual_bounded(a, out)?;
                out.push_str(" not ")?;
                self.write_name_bounded(&Entity::DataProperty(p.clone()), out)?;
                out.push_char(' ')?;
                self.write_literal_bounded(l, out)?;
            }
            A::SubAnnotationPropertyOf(a, b) => {
                self.write_name_bounded(&Entity::AnnotationProperty(a.clone()), out)?;
                out.push_str(" SubPropertyOf ")?;
                self.write_name_bounded(&Entity::AnnotationProperty(b.clone()), out)?;
            }
            A::AnnotationPropertyDomain(p, i) => {
                self.write_name_bounded(&Entity::AnnotationProperty(p.clone()), out)?;
                out.push_str(" Domain ")?;
                self.write_iri_bounded(i.as_str(), out)?;
            }
            A::AnnotationPropertyRange(p, i) => {
                self.write_name_bounded(&Entity::AnnotationProperty(p.clone()), out)?;
                out.push_str(" Range ")?;
                self.write_iri_bounded(i.as_str(), out)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

struct BoundedBuf {
    buf: String,
    limit: usize,
}

impl BoundedBuf {
    fn new(limit: usize) -> Self {
        Self {
            buf: String::new(),
            limit,
        }
    }

    fn remaining(&self) -> usize {
        self.limit.saturating_sub(self.buf.len())
    }

    fn push_str(&mut self, s: &str) -> Result<(), ()> {
        if self.buf.len().saturating_add(s.len()) > self.limit {
            return Err(());
        }
        self.buf.push_str(s);
        Ok(())
    }

    fn push_char(&mut self, c: char) -> Result<(), ()> {
        let mut tmp = [0u8; 4];
        self.push_str(c.encode_utf8(&mut tmp))
    }

    fn push_u32(&mut self, mut n: u32) -> Result<(), ()> {
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
        self.push_str(std::str::from_utf8(&buf[i..]).expect("ascii"))
    }
}

/// Escape a W3C Manchester `quotedString`, escaping only `"` and `\` and preserving
/// literal newlines and tabs inside the quotes.
fn quote_manchester_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether a literal of `datatype` is written bare: an integer or a decimal with digits
/// on both sides of its point, whose lexical form reads back as itself under W3C
/// Manchester syntax.
pub fn bare_literal(value: &str, datatype: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let unsigned = value.strip_prefix(['+', '-']).unwrap_or(value);
    match datatype {
        vocab::XSD_INTEGER => digits(unsigned),
        vocab::XSD_DECIMAL => unsigned
            .split_once('.')
            .is_some_and(|(a, b)| digits(a) && digits(b)),
        _ => false,
    }
}

/// The word of a facet: `<=`, `<`, `>=` and `>` for the bounds, the local name for the
/// others.
fn facet_word(facet: &str) -> &str {
    match super::local_name(facet) {
        "minInclusive" => ">=",
        "maxInclusive" => "<=",
        "minExclusive" => ">",
        "maxExclusive" => "<",
        local => local,
    }
}
