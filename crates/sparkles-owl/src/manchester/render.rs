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
            && let Found::Entities(es) =
                v.lookup(&lex::Name::Quoted(label.to_string()), &[e.kind()])
            && es.len() == 1
            && es[0].iri().as_str() == iri
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

    /// A literal: a quoted string with its language tag or datatype, or a bare integer,
    /// decimal or boolean when its lexical form reads back as one.
    pub fn literal(&self, l: &Literal) -> String {
        let v = serde_json::to_string(l.value()).expect("a string serializes");
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
            Ce::ObjectComplementOf(c) => format!("not {}", m(c)),
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
            DataRange::ComplementOf(r) => format!("not {}", self.dr(r, true)),
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
}

/// Whether a literal of `datatype` is written bare: an integer, a decimal with digits
/// on both sides of its point, or a boolean, whose lexical form reads back as itself.
pub fn bare_literal(value: &str, datatype: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let unsigned = value.strip_prefix(['+', '-']).unwrap_or(value);
    match datatype {
        vocab::XSD_INTEGER => digits(unsigned),
        vocab::XSD_DECIMAL => unsigned
            .split_once('.')
            .is_some_and(|(a, b)| digits(a) && digits(b)),
        vocab::XSD_BOOLEAN => value == "true" || value == "false",
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
