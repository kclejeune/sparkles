//! The OWL 2 Manchester Syntax (W3C Working Group Note, second edition) for class
//! expressions, data ranges, individuals and single axioms.
//!
//! [`class_expression`], [`data_range`], [`individual`] and [`axiom`] read text against
//! a [`Vocabulary`], which holds the prefixes, the kinds of the entities and their names.
//! A name is a full IRI in angle brackets, a prefixed name, or a simple name that is an
//! entity's `rdfs:label` or `skos:prefLabel` or the local name of its IRI. A name with
//! spaces or other characters a bare word cannot hold is quoted with single quotes, as
//! in `'Graduate student'`. A name that two entities of the wanted kinds carry is an
//! [`ErrorKind::Ambiguous`] error that lists them, and every error carries the byte
//! [`Span`] of the text it is about.
//!
//! The grammar is that of the Manchester Syntax note: `or` binds weaker than `and`, which
//! binds weaker than `not`, and a restriction's filler is a primary, so that
//! `p some A and B` reads as `(p some A) and B`. `that` is read as `and`. Literals are
//! quoted strings with a language tag or a datatype, integers, decimals, floating-point
//! numbers with an `f` suffix, and, beyond the note, `true` and `false` as `xsd:boolean`.
//! An axiom is written with the keyword of its frame section between its parts, such as
//! `A SubClassOf B`, `p Domain C`, `a Type C`, `p o q SubPropertyOf r` or `a p b` for
//! a fact.
//!
//! [`Renderer`] writes the same syntax, and parsing what it writes gives back the
//! expression or axiom it wrote.
//!
//! ```
//! use sparkles_owl::manchester::{self, Vocabulary};
//! use sparkles_owl::model::*;
//!
//! let mut v = Vocabulary::new([("ex", "http://ex.org/")]);
//! v.add_entity(Entity::Class(Class::new("http://ex.org/Cell")));
//! v.add_entity(Entity::ObjectProperty(ObjectProperty::new("http://ex.org/partOf")));
//! v.add_label("http://ex.org/partOf", "part of");
//! let c = manchester::class_expression("'part of' some ex:Cell", &v).unwrap();
//! assert_eq!(
//!     c,
//!     ClassExpression::some(ObjectProperty::new("http://ex.org/partOf"), Class::new("http://ex.org/Cell")),
//! );
//! let e = manchester::class_expression("'part of' some Cel", &v).unwrap_err();
//! assert_eq!((e.span.start, e.span.end), (18, 21));
//! ```

mod lex;
mod parse;
mod render;

pub use render::{Renderer, quote_name};

use crate::model::*;
use crate::read::Ontology;
use crate::vocab;
use rustc_hash::FxHashMap;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fmt;

/// A byte range of the text read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        Span { start, end }
    }

    /// The span in UTF-16 code units of `text`, as a browser's text field counts them.
    pub fn utf16(&self, text: &str) -> (usize, usize) {
        let at = |b: usize| {
            let b = b.min(text.len());
            // a span always falls on character boundaries, but be safe
            let mut b = b;
            while !text.is_char_boundary(b) {
                b -= 1;
            }
            text[..b].encode_utf16().count()
        };
        (at(self.start), at(self.end))
    }
}

/// What a [`Error`] is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// the text does not follow the grammar
    Syntax,
    /// no entity has the name
    Unknown,
    /// several entities have the name
    Ambiguous,
    /// the name is an entity of another kind than the place wants
    Kind,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Syntax => "syntax",
            ErrorKind::Unknown => "unknown",
            ErrorKind::Ambiguous => "ambiguous",
            ErrorKind::Kind => "kind",
        }
    }
}

/// An entity a name could stand for, with its label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub entity: Entity,
    pub label: Option<String>,
}

/// An error of a parse, with the span of the text it is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    pub span: Span,
    /// the entities an ambiguous name could stand for, or those of another kind that an
    /// unknown name has
    pub candidates: Vec<Candidate>,
}

impl Error {
    pub(crate) fn syntax(span: Span, message: impl Into<String>) -> Error {
        Error {
            kind: ErrorKind::Syntax,
            message: message.into(),
            span,
            candidates: Vec::new(),
        }
    }

    /// The JSON form of the error for `text`, the text parsed: its message, its kind as
    /// `reason`, its span in UTF-16 code units (`start`, `end`) and its candidates, each
    /// with its IRI, its kind and its label.
    pub fn json(&self, text: &str) -> Value {
        let (start, end) = self.span.utf16(text);
        json!({
            "error": self.message,
            "code": "invalid-manchester",
            "reason": self.kind.as_str(),
            "start": start,
            "end": end,
            "candidates": self.candidates.iter().map(|c| json!({
                "iri": c.entity.iri().as_str(),
                "kind": c.entity.kind().name(),
                "label": c.label,
            })).collect::<Vec<_>>(),
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (at {}..{})",
            self.message, self.span.start, self.span.end
        )
    }
}

impl std::error::Error for Error {}

/// The prefixes, entity kinds and names that Manchester text is read against.
#[derive(Clone, Debug, Default)]
pub struct Vocabulary {
    /// `(prefix, namespace)`, the standard ones included
    prefixes: Vec<(String, String)>,
    kinds: FxHashMap<String, Vec<EntityKind>>,
    /// labels and local names, to the IRIs that carry them
    names: FxHashMap<String, BTreeSet<String>>,
    /// the same by their lower case, for a name typed in another case
    folded: FxHashMap<String, BTreeSet<String>>,
    /// the simple names of built-in entities, such as `Thing` and `integer`
    builtin: FxHashMap<&'static str, String>,
    /// the first label of each IRI, for candidates
    label: FxHashMap<String, String>,
}

/// The kinds of a name a lookup found.
pub(crate) enum Found {
    /// entities of the wanted kinds, each IRI with its kinds in the wanted order
    Entities(Vec<Entity>),
    /// none of the wanted kinds: the IRI the name stands for, when it names one, and the
    /// entities of other kinds that it names
    Unknown {
        iri: Option<String>,
        others: Vec<Entity>,
    },
}

impl Vocabulary {
    /// A vocabulary with `prefixes` and the standard `owl:`, `rdf:`, `rdfs:` and `xsd:`
    /// ones, and the built-in entities: `owl:Thing`, `owl:Nothing`, the top and bottom
    /// properties, the datatypes of the OWL 2 datatype map and the built-in annotation
    /// properties.
    pub fn new<P: Into<String>, N: Into<String>>(
        prefixes: impl IntoIterator<Item = (P, N)>,
    ) -> Vocabulary {
        let mut v = Vocabulary {
            prefixes: prefixes
                .into_iter()
                .map(|(p, n)| (p.into(), n.into()))
                .collect(),
            ..Default::default()
        };
        for (p, n) in crate::ofn::STANDARD_PREFIXES {
            if !v.prefixes.iter().any(|(q, _)| q == p) {
                v.prefixes.push((p.to_string(), n.to_string()));
            }
        }
        let builtins = [
            (vocab::OWL_THING, EntityKind::Class),
            (vocab::OWL_NOTHING, EntityKind::Class),
            (vocab::OWL_TOP_OBJECT_PROPERTY, EntityKind::ObjectProperty),
            (
                vocab::OWL_BOTTOM_OBJECT_PROPERTY,
                EntityKind::ObjectProperty,
            ),
            (vocab::OWL_TOP_DATA_PROPERTY, EntityKind::DataProperty),
            (vocab::OWL_BOTTOM_DATA_PROPERTY, EntityKind::DataProperty),
            (vocab::RDFS_LITERAL, EntityKind::Datatype),
        ];
        for (iri, k) in builtins {
            v.kind(iri, k);
        }
        for d in vocab::DATATYPE_MAP {
            v.kind(d, EntityKind::Datatype);
        }
        for a in vocab::BUILTIN_ANNOTATION_PROPERTIES {
            v.kind(a, EntityKind::AnnotationProperty);
        }
        for (w, iri) in [
            ("Thing", vocab::OWL_THING),
            ("Nothing", vocab::OWL_NOTHING),
            ("integer", vocab::XSD_INTEGER),
            ("decimal", vocab::XSD_DECIMAL),
            ("float", vocab::XSD_FLOAT),
            ("string", vocab::XSD_STRING),
        ] {
            v.builtin.insert(w, iri.to_string());
        }
        v
    }

    /// The vocabulary of a read ontology with `prefixes`: its entities, as
    /// [`Ontology::entities`] lists them, and the `rdfs:label` and `skos:prefLabel` of
    /// each.
    pub fn of_ontology<P: Into<String>, N: Into<String>>(
        o: &Ontology,
        prefixes: impl IntoIterator<Item = (P, N)>,
    ) -> Vocabulary {
        let mut v = Vocabulary::new(prefixes);
        for k in EntityKind::ALL {
            for e in o.entities(k) {
                v.add_entity(e);
            }
        }
        for ax in o.axiom_set() {
            if let AxiomKind::AnnotationAssertion(
                p,
                AnnotationSubject::Iri(s),
                AnnotationValue::Literal(l),
            ) = &ax.kind
                && LABEL_PROPERTIES.contains(&p.as_str())
            {
                v.add_label(s.as_str(), l.value());
            }
        }
        v
    }

    fn kind(&mut self, iri: &str, k: EntityKind) {
        let ks = self.kinds.entry(iri.to_string()).or_default();
        if !ks.contains(&k) {
            ks.push(k);
        }
    }

    fn name(&mut self, name: &str, iri: &str) {
        self.names
            .entry(name.to_string())
            .or_default()
            .insert(iri.to_string());
        self.folded
            .entry(name.to_lowercase())
            .or_default()
            .insert(iri.to_string());
    }

    /// Adds an entity, which its IRI's local name then names.
    pub fn add_entity(&mut self, e: Entity) {
        let iri = e.iri().as_str().to_string();
        self.kind(&iri, e.kind());
        if !e.is_builtin() {
            let local = local_name(&iri);
            if !local.is_empty() {
                self.name(local, &iri);
            }
        }
    }

    /// Adds a label of the entity `iri`, which then names it.
    pub fn add_label(&mut self, iri: &str, label: &str) {
        if label.trim().is_empty() {
            return;
        }
        self.name(label, iri);
        self.label
            .entry(iri.to_string())
            .or_insert_with(|| label.to_string());
    }

    /// The kinds of the entity `iri`, as declared or built in.
    pub fn kinds(&self, iri: &str) -> &[EntityKind] {
        self.kinds.get(iri).map_or(&[], |k| k.as_slice())
    }

    /// The prefixes, the standard ones included.
    pub fn prefixes(&self) -> &[(String, String)] {
        &self.prefixes
    }

    /// The first label of `iri` that was added.
    pub fn label(&self, iri: &str) -> Option<&str> {
        self.label.get(iri).map(String::as_str)
    }

    /// The IRIs that the simple name `name` stands for exactly.
    pub fn named(&self, name: &str) -> impl Iterator<Item = &str> {
        self.names
            .get(name)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }

    pub(crate) fn candidate(&self, e: &Entity) -> Candidate {
        Candidate {
            entity: e.clone(),
            label: self.label(e.iri().as_str()).map(str::to_string),
        }
    }

    /// The IRI a prefixed name expands to, when its prefix is known.
    pub fn expand(&self, word: &str) -> Option<String> {
        let (p, local) = word.split_once(':')?;
        if local.starts_with("//") {
            return None;
        }
        self.prefixes
            .iter()
            .find(|(q, _)| q == p)
            .map(|(_, ns)| format!("{ns}{local}"))
    }

    fn by_iri(&self, iri: &str, want: &[EntityKind]) -> Found {
        let have = self.kinds(iri);
        let hits: Vec<Entity> = want
            .iter()
            .filter(|k| have.contains(k))
            .map(|k| Entity::of_kind(*k, crate::model::iri(iri)))
            .collect();
        if hits.is_empty() {
            Found::Unknown {
                iri: Some(iri.to_string()),
                others: have
                    .iter()
                    .map(|k| Entity::of_kind(*k, crate::model::iri(iri)))
                    .collect(),
            }
        } else {
            Found::Entities(hits)
        }
    }

    fn by_names<'a>(
        &self,
        iris: impl IntoIterator<Item = &'a String>,
        want: &[EntityKind],
    ) -> (Vec<Entity>, Vec<Entity>) {
        let mut hits = Vec::new();
        let mut others = Vec::new();
        for iri in iris {
            match self.by_iri(iri, want) {
                Found::Entities(es) => hits.extend(es),
                Found::Unknown { others: o, .. } => others.extend(o),
            }
        }
        (hits, others)
    }

    /// The entities of the wanted kinds that a name stands for. A full IRI or a prefixed
    /// name stands for its IRI, and a label or a local name for the entities that carry
    /// it: exactly, then among the built-in names, then in any case. A bare word with
    /// the empty prefix's namespace stands for that IRI as a last resort.
    pub(crate) fn lookup(&self, n: &lex::Name, want: &[EntityKind]) -> Found {
        let simple = match n {
            lex::Name::Iri(i) => return self.by_iri(i, want),
            lex::Name::Word(w) => {
                if let Some(iri) = self.expand(w) {
                    if !self.kinds(&iri).is_empty() {
                        return self.by_iri(&iri, want);
                    }
                    // a label with a colon in it, or an IRI the ontology does not know
                    let (hits, _) = self.by_names(self.names.get(w).into_iter().flatten(), want);
                    if !hits.is_empty() {
                        return Found::Entities(hits);
                    }
                    return self.by_iri(&iri, want);
                }
                w.as_str()
            }
            lex::Name::Quoted(q) => q.as_str(),
        };
        let (hits, mut others) = self.by_names(self.names.get(simple).into_iter().flatten(), want);
        if !hits.is_empty() {
            return Found::Entities(hits);
        }
        if let Some(iri) = self.builtin.get(simple) {
            let (hits, _) = self.by_names([iri], want);
            if !hits.is_empty() {
                return Found::Entities(hits);
            }
        }
        let (hits, o) = self.by_names(
            self.folded
                .get(&simple.to_lowercase())
                .into_iter()
                .flatten(),
            want,
        );
        if !hits.is_empty() {
            return Found::Entities(hits);
        }
        if others.is_empty() {
            others = o;
        }
        if matches!(n, lex::Name::Word(_))
            && let Some(iri) = self.expand(&format!(":{simple}"))
            && !self.kinds(&iri).is_empty()
        {
            return self.by_iri(&iri, want);
        }
        Found::Unknown { iri: None, others }
    }
}

/// The annotation properties whose values name an entity.
pub const LABEL_PROPERTIES: [&str; 2] = [
    vocab::RDFS_LABEL,
    "http://www.w3.org/2004/02/skos/core#prefLabel",
];

/// The local name of an IRI: what follows its last `#`, `/` or `:`.
pub fn local_name(iri: &str) -> &str {
    iri.rfind(['#', '/', ':']).map_or(iri, |i| &iri[i + 1..])
}

/// Read a class expression.
pub fn class_expression(text: &str, v: &Vocabulary) -> Result<ClassExpression, Error> {
    parse::Parser::new(text, v)?.whole(|p| p.description())
}

/// Read a data range.
pub fn data_range(text: &str, v: &Vocabulary) -> Result<DataRange, Error> {
    parse::Parser::new(text, v)?.whole(|p| p.data_range())
}

/// Read an individual: a name or an anonymous individual such as `_:b1`.
pub fn individual(text: &str, v: &Vocabulary) -> Result<Individual, Error> {
    parse::Parser::new(text, v)?.whole(|p| p.individual())
}

/// Read a literal.
pub fn literal(text: &str, v: &Vocabulary) -> Result<Literal, Error> {
    parse::Parser::new(text, v)?.whole(|p| p.literal())
}

/// Read one axiom, such as `A SubClassOf p some B`. The forms are those of
/// [`Renderer::axiom`]: `SubClassOf`, `EquivalentTo` and `DisjointWith` between two
/// class expressions or two properties, `SubPropertyOf` from a property or a chain
/// `p o q`, `InverseOf`, `Domain` and `Range` of a property, `Type`, `SameAs` and
/// `DifferentFrom` of an individual, a characteristic such as `Transitive p`, and a
/// fact `a p b` or `a not p b`.
pub fn axiom(text: &str, v: &Vocabulary) -> Result<Axiom, Error> {
    parse::Parser::new(text, v)?.whole(|p| p.axiom())
}

/// What [`parse`] reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum What {
    ClassExpression,
    DataRange,
    Individual,
    Literal,
    Axiom,
}

impl What {
    pub fn parse(s: &str) -> Option<What> {
        Some(match s {
            "classExpression" => What::ClassExpression,
            "dataRange" => What::DataRange,
            "individual" => What::Individual,
            "literal" => What::Literal,
            "axiom" => What::Axiom,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            What::ClassExpression => "classExpression",
            What::DataRange => "dataRange",
            What::Individual => "individual",
            What::Literal => "literal",
            What::Axiom => "axiom",
        }
    }

    pub const ALL: [What; 5] = [
        What::ClassExpression,
        What::DataRange,
        What::Individual,
        What::Literal,
        What::Axiom,
    ];
}

/// A value [`parse`] read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    ClassExpression(ClassExpression),
    DataRange(DataRange),
    Individual(Individual),
    Literal(Literal),
    Axiom(Axiom),
}

impl Parsed {
    /// The JSON form of the value, as [`crate::json`] writes it.
    pub fn json(&self) -> Value {
        match self {
            Parsed::ClassExpression(c) => crate::json::class_expression(c),
            Parsed::DataRange(r) => crate::json::data_range(r),
            Parsed::Individual(i) => crate::json::individual(i),
            Parsed::Literal(l) => crate::json::literal(l),
            Parsed::Axiom(a) => crate::json::axiom(a),
        }
    }
}

/// Read `text` as `what`.
pub fn parse(text: &str, what: What, v: &Vocabulary) -> Result<Parsed, Error> {
    Ok(match what {
        What::ClassExpression => Parsed::ClassExpression(class_expression(text, v)?),
        What::DataRange => Parsed::DataRange(data_range(text, v)?),
        What::Individual => Parsed::Individual(individual(text, v)?),
        What::Literal => Parsed::Literal(literal(text, v)?),
        What::Axiom => Parsed::Axiom(axiom(text, v)?),
    })
}

#[cfg(test)]
mod tests;
